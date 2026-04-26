# Cluufile Mount-Policy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the hardcoded per-container `/tmp` swap with a declarative Cluufile `MOUNT <path> <policy>` directive, so nested containers can inherit `/tmp` from their shell session (unblocking Shell-A Plan 2's multi-spawn filesystem tests).

**Architecture:** (1) Extend the Cluufile parser with a new `MOUNT` directive that emits a `[[mounts.policy]]` table into `manifest.toml`. (2) Extend the VFS `VFS_SET_VIEW` wire format with a per-mount `memfs_cid` (u64) so procmgr — not VFS — decides which container's MemFs each mount resolves to. (3) Replace the unconditional container-isolation prepend in VFS with procmgr-driven explicit mounts governed by policy (defaults: `/tmp → inherit`, `/log → private`, others preserve today's behavior).

**Tech Stack:** Rust (no_std userspace binaries, host-side CLI), TOML manifests (`target/containers/*/manifest.toml`), bash harness (`scripts/harness_*.sh`), QEMU integration tests.

**Note on spec deviation:** The spec (`docs/superpowers/specs/2026-04-23-mount-policy-design.md` §"VFS side") claims VFS needs no logic change. That's wrong — VFS today unconditionally prepends `/tmp`, `/log`, `/data`, and `/` catch-all mounts keyed by `container_id` for every set_view with `container_id > 0` (vfs/main.rs:706–759). That block is the real enforcement point for `/tmp` isolation, not just the procmgr swap at 4583-4593. This plan replaces that VFS block with procmgr-provided explicit mounts.

---

## File Structure

**Created files:**
- `containers/mountprobe/Cluufile` — test harness container with `MOUNT /tmp private`
- `containers/mountprobe/Cargo.toml` — probe crate manifest
- `userspace/mountprobe/src/main.rs` — probe binary that verifies `/tmp` isolation

**Modified files:**
- `tools/container-build/src/main.rs` — Cluufile parser: MOUNT directive, validation, manifest emission
- `userspace/procmgr/src/main.rs` — manifest consumer, policy resolution, view builder, VFS wire serializer
- `userspace/vfs/src/main.rs` — VFS wire parser (memfs_cid), drop unconditional prepend
- `containers/shell/Cluufile` — add explicit `MOUNT /tmp private` session anchor
- `scripts/harness_cases.conf` — register `l2_mount_private` case
- `scripts/harness_case_defaults.sh` — autostart command for `l2_mount_private`
- `scripts/harness_run.sh` — required_markers for `l2_mount_private`
- `xtask/src/main.rs` — build registration for `mountprobe` container (if xtask has a container list)
- `userspace/libcluu/src/ipc.rs` — bump `VFS_SET_VIEW_LABEL` wire version or document layout change

**Unchanged (by design):**
- `kernel/**` — zero kernel changes
- MemFs backend implementation (`userspace/vfs/src/mount.rs`) — just reused with new keying

---

## Scope Check

Single subsystem (container VFS view construction). One plan, one implementation cycle. The optional `ro` policy from the spec is deferred — `inherit` and `private` alone unblock Plan 2.

---

## Task 1: Cluufile parser — add MOUNT directive (parsing + field storage)

**Files:**
- Modify: `tools/container-build/src/main.rs:42-58` (Cluufile struct)
- Modify: `tools/container-build/src/main.rs:60-364` (parse_cluufile)
- Modify: `tools/container-build/Cargo.toml` (no change expected, but verify dev-deps section)

- [ ] **Step 1: Add `mount_policies` field to `Cluufile` struct**

Edit `tools/container-build/src/main.rs` — add to the `Cluufile` struct (after `restart_policy`):

```rust
#[derive(Debug, Clone)]
struct Cluufile {
    base: String,
    profile: Vec<String>,
    entrypoint: Vec<String>,
    builds: Vec<BuildStep>,
    copies: Vec<(String, String)>,
    persistent_dirs: Vec<String>,
    env: Vec<(String, String)>,
    priority: Option<usize>,
    endpoint_mode: Option<String>,
    params: Vec<String>,
    devices: Vec<String>,
    deny_inherit: bool,
    deny: Vec<String>,
    detach: bool,
    restart_policy: Option<(String, Option<usize>, Option<u64>)>,
    /// MOUNT directives: (path, policy) where policy ∈ {"inherit", "private", "ro"}.
    /// Duplicate paths are a parse error (caught in parse_cluufile).
    mount_policies: Vec<(String, String)>,
}
```

And initialize it in the `Ok(Cluufile { ... })` block at the end of `parse_cluufile` (currently around line 347):

```rust
Ok(Cluufile {
    base,
    profile: profile.unwrap_or_default(),
    entrypoint: entrypoint.unwrap_or_default(),
    builds,
    copies,
    persistent_dirs,
    env,
    priority,
    endpoint_mode,
    params,
    devices,
    deny_inherit,
    deny,
    detach,
    restart_policy,
    mount_policies,
})
```

Also add the accumulator variable at the top of `parse_cluufile`, alongside the other `let mut` declarations (after line 78):

```rust
let mut mount_policies: Vec<(String, String)> = Vec::new();
```

- [ ] **Step 2: Add `MOUNT` directive branch to parser match**

Inside the `match directive { ... }` block in `parse_cluufile`, add a new arm before the `unknown =>` catch-all (around line 332):

```rust
"MOUNT" => {
    if base.is_none() {
        bail!("{}:{}: FROM must appear before MOUNT", path.display(), lineno);
    }
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.len() != 2 {
        bail!(
            "{}:{}: MOUNT requires exactly two arguments (path policy), got {}",
            path.display(), lineno, tokens.len()
        );
    }
    let mount_path = tokens[0].to_string();
    let policy = tokens[1].to_string();
    match policy.as_str() {
        "inherit" | "private" | "ro" => {}
        other => {
            bail!(
                "{}:{}: MOUNT policy must be 'inherit', 'private', or 'ro', got '{}'",
                path.display(), lineno, other
            );
        }
    }
    if !mount_path.starts_with('/') {
        bail!(
            "{}:{}: MOUNT path must be absolute, got '{}'",
            path.display(), lineno, mount_path
        );
    }
    if mount_policies.iter().any(|(p, _)| p == &mount_path) {
        bail!(
            "{}:{}: duplicate MOUNT directive for path '{}'",
            path.display(), lineno, mount_path
        );
    }
    mount_policies.push((mount_path, policy));
}
```

- [ ] **Step 3: Add unit test module at bottom of `tools/container-build/src/main.rs`**

At the very end of the file, add:

```rust
#[cfg(test)]
mod mount_tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn parse_from_string(content: &str) -> Result<Cluufile> {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        parse_cluufile(tmp.path())
    }

    #[test]
    fn mount_directive_parses_inherit() {
        let src = "FROM base\nMOUNT /tmp inherit\n";
        let c = parse_from_string(src).expect("should parse");
        assert_eq!(c.mount_policies, vec![("/tmp".to_string(), "inherit".to_string())]);
    }

    #[test]
    fn mount_directive_parses_private() {
        let src = "FROM base\nMOUNT /log private\n";
        let c = parse_from_string(src).expect("should parse");
        assert_eq!(c.mount_policies, vec![("/log".to_string(), "private".to_string())]);
    }

    #[test]
    fn mount_directive_rejects_unknown_policy() {
        let src = "FROM base\nMOUNT /tmp shared\n";
        let err = parse_from_string(src).expect_err("shared is not a valid policy");
        assert!(err.to_string().contains("MOUNT policy must be"), "err was: {}", err);
    }

    #[test]
    fn mount_directive_rejects_relative_path() {
        let src = "FROM base\nMOUNT tmp inherit\n";
        let err = parse_from_string(src).expect_err("relative path should fail");
        assert!(err.to_string().contains("MOUNT path must be absolute"), "err was: {}", err);
    }

    #[test]
    fn mount_directive_rejects_duplicate_path() {
        let src = "FROM base\nMOUNT /tmp inherit\nMOUNT /tmp private\n";
        let err = parse_from_string(src).expect_err("duplicate MOUNT should fail");
        assert!(err.to_string().contains("duplicate MOUNT"), "err was: {}", err);
    }

    #[test]
    fn mount_directive_rejects_wrong_arity() {
        let src = "FROM base\nMOUNT /tmp\n";
        let err = parse_from_string(src).expect_err("single-arg MOUNT should fail");
        assert!(err.to_string().contains("MOUNT requires exactly two arguments"), "err was: {}", err);
    }

    #[test]
    fn mount_directive_rejects_before_from() {
        let src = "MOUNT /tmp inherit\nFROM base\n";
        let err = parse_from_string(src).expect_err("MOUNT before FROM should fail");
        assert!(err.to_string().contains("FROM must appear before MOUNT"), "err was: {}", err);
    }

    #[test]
    fn multiple_mount_directives_accumulate() {
        let src = "FROM base\nMOUNT /tmp inherit\nMOUNT /log private\n";
        let c = parse_from_string(src).expect("should parse");
        assert_eq!(c.mount_policies.len(), 2);
        assert_eq!(c.mount_policies[0].0, "/tmp");
        assert_eq!(c.mount_policies[1].0, "/log");
    }
}
```

- [ ] **Step 4: Add `tempfile` dev-dependency**

Edit `tools/container-build/Cargo.toml` — add to the file (create `[dev-dependencies]` section if missing):

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 5: Run the failing tests**

Run: `cargo test -p container-build mount_tests`
Expected: Tests compile but fail at the `assert_eq!(c.mount_policies, ...)` line because the parser arm isn't wired yet.

Wait — if you ordered Step 1, 2, 3, 4 correctly, they'll all pass on first run. To demonstrate TDD, comment out the `"MOUNT" => { ... }` arm temporarily, run the tests (they should fail with "unknown directive 'MOUNT'"), then uncomment and re-run.

Run: `cargo test -p container-build mount_tests`
Expected after re-enabling: 8 passed, 0 failed.

- [ ] **Step 6: Commit**

```bash
git add tools/container-build/src/main.rs tools/container-build/Cargo.toml
git commit -m "container-build: parse MOUNT directive in Cluufile

New directive: MOUNT <path> <policy> where policy ∈ {inherit, private, ro}.
Validates absolute path, known policy, no duplicates. Stored on Cluufile
struct for later consumption by manifest emitter and procmgr.

Unit tests cover happy paths and all validation branches."
```

---

## Task 2: Cluufile parser — emit `[[mounts.policy]]` into manifest.toml

**Files:**
- Modify: `tools/container-build/src/main.rs:370-509` (generate_manifest_toml)

- [ ] **Step 1: Add failing unit test**

Append to `mod mount_tests` in `tools/container-build/src/main.rs`:

```rust
    #[test]
    fn manifest_emits_mount_policy_entries() {
        let src = "FROM base\nMOUNT /tmp inherit\nMOUNT /log private\n";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        assert!(toml.contains("[[mounts.policy]]"), "missing section: {}", toml);
        assert!(toml.contains("path = \"/tmp\""), "missing path: {}", toml);
        assert!(toml.contains("policy = \"inherit\""), "missing policy: {}", toml);
        assert!(toml.contains("path = \"/log\""), "missing path: {}", toml);
        assert!(toml.contains("policy = \"private\""), "missing policy: {}", toml);
    }

    #[test]
    fn manifest_omits_mount_section_when_no_policies() {
        let src = "FROM base\n";
        let c = parse_from_string(src).expect("should parse");
        let toml = generate_manifest_toml(&c, "test", &[]);
        assert!(!toml.contains("[[mounts.policy]]"), "should not have section: {}", toml);
    }
```

- [ ] **Step 2: Run test and verify failure**

Run: `cargo test -p container-build manifest_emits_mount_policy`
Expected: FAIL — the emitter doesn't produce `[[mounts.policy]]` yet.

- [ ] **Step 3: Extend `generate_manifest_toml` to emit mount policy entries**

In `tools/container-build/src/main.rs`, locate the `[mounts]` section emission block (currently around lines 490-506). Extend it so that `mount_policies` entries produce additional `[[mounts.policy]]` tables. Replace the existing block:

```rust
    // [mounts] — only if deny_inherit or deny paths specified
    if cluufile.deny_inherit || !cluufile.deny.is_empty() {
        out.push_str("\n[mounts]\n");
        if cluufile.deny_inherit {
            out.push_str("deny_inherit = true\n");
        }
        if !cluufile.deny.is_empty() {
            out.push_str("deny = [");
            for (i, path) in cluufile.deny.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("\"{}\"", path));
            }
            out.push_str("]\n");
        }
    }
```

With:

```rust
    // [mounts] — emitted if deny_inherit, deny paths, or mount policies are set.
    // Mount policies are emitted as [[mounts.policy]] array-of-tables so procmgr
    // can read them as a vector without ambiguity versus deny_inherit / deny.
    let has_mount_section = cluufile.deny_inherit
        || !cluufile.deny.is_empty()
        || !cluufile.mount_policies.is_empty();
    if has_mount_section {
        out.push_str("\n[mounts]\n");
        if cluufile.deny_inherit {
            out.push_str("deny_inherit = true\n");
        }
        if !cluufile.deny.is_empty() {
            out.push_str("deny = [");
            for (i, path) in cluufile.deny.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("\"{}\"", path));
            }
            out.push_str("]\n");
        }
        for (path, policy) in &cluufile.mount_policies {
            out.push_str(&format!(
                "\n[[mounts.policy]]\npath = \"{}\"\npolicy = \"{}\"\n",
                path, policy
            ));
        }
    }
```

- [ ] **Step 4: Run test to confirm pass**

Run: `cargo test -p container-build manifest_emits_mount_policy manifest_omits_mount_section`
Expected: both pass.

- [ ] **Step 5: Commit**

```bash
git add tools/container-build/src/main.rs
git commit -m "container-build: emit [[mounts.policy]] into manifest.toml

MOUNT directives now produce [[mounts.policy]] array-of-tables alongside
the existing [mounts] table keys (deny_inherit, deny). Procmgr will
consume these to drive the per-container view policy at spawn time."
```

---

## Task 3: Cluufile parser — conflict validation (DENY and PERSISTENT)

**Files:**
- Modify: `tools/container-build/src/main.rs:60-364` (parse_cluufile — add post-parse validation)

- [ ] **Step 1: Add failing unit tests**

Append to `mod mount_tests`:

```rust
    #[test]
    fn mount_conflicts_with_deny() {
        let src = "FROM base\nDENY /tmp\nMOUNT /tmp inherit\n";
        let err = parse_from_string(src).expect_err("MOUNT on DENY path should fail");
        assert!(
            err.to_string().contains("MOUNT conflicts with DENY"),
            "err was: {}", err
        );
    }

    #[test]
    fn mount_conflicts_with_persistent() {
        let src = "FROM base\nPERSISTENT /data\nMOUNT /data private\n";
        let err = parse_from_string(src).expect_err("MOUNT on PERSISTENT path should fail");
        assert!(
            err.to_string().contains("MOUNT conflicts with PERSISTENT"),
            "err was: {}", err
        );
    }

    #[test]
    fn deny_declared_after_mount_still_conflicts() {
        let src = "FROM base\nMOUNT /tmp inherit\nDENY /tmp\n";
        let err = parse_from_string(src).expect_err("order shouldn't matter");
        assert!(
            err.to_string().contains("MOUNT conflicts with DENY"),
            "err was: {}", err
        );
    }
```

- [ ] **Step 2: Run tests to confirm failure**

Run: `cargo test -p container-build mount_conflicts deny_declared_after`
Expected: FAIL — no conflict check exists yet.

- [ ] **Step 3: Add post-parse validation**

At the bottom of `parse_cluufile` in `tools/container-build/src/main.rs`, **after** the `for (line_idx, raw_line)` loop has finished but **before** the `let base = base.ok_or_else(...)` line (around line 343), insert conflict validation:

```rust
    // Post-parse validation: MOUNT must not overlap with DENY or PERSISTENT.
    // Both orderings caught because we check after the whole file is parsed.
    for (mount_path, _) in &mount_policies {
        if deny.iter().any(|d| d == mount_path) {
            bail!(
                "{}: MOUNT conflicts with DENY for path '{}' (ambiguous intent)",
                path.display(), mount_path
            );
        }
        if persistent_dirs.iter().any(|p| p == mount_path) {
            bail!(
                "{}: MOUNT conflicts with PERSISTENT for path '{}' (PERSISTENT already implies private)",
                path.display(), mount_path
            );
        }
    }
```

- [ ] **Step 4: Run tests to confirm pass**

Run: `cargo test -p container-build mount_conflicts deny_declared_after`
Expected: 3 tests pass.

Also run: `cargo test -p container-build` (all tests)
Expected: all prior Task 1/2 tests still pass.

- [ ] **Step 5: Commit**

```bash
git add tools/container-build/src/main.rs
git commit -m "container-build: validate MOUNT conflicts with DENY and PERSISTENT

DENY <path> and PERSISTENT <path> already carry mount-intent semantics
(filter-out and private-ext2 respectively). A MOUNT directive on the same
path would be ambiguous, so flag it as a parse error regardless of
declaration order."
```

---

## Task 4: procmgr — parse `[[mounts.policy]]` from manifest.toml

**Files:**
- Modify: `userspace/procmgr/src/main.rs:4380-4410` (manifest-parsing area of handle_container_run)

- [ ] **Step 1: Check current manifest-parsing shape**

Read `userspace/procmgr/src/main.rs:4380-4410`. You'll see `deny_inherit` and `deny_paths` read from `doc.table("mounts")`. We'll add a third read: a vector of `(path, policy)` tuples.

Also check what TOML library procmgr uses. Grep: `Grep "use.*toml" userspace/procmgr/src/main.rs`. Procmgr uses a homegrown TOML reader — look for `Document` or similar type and examine its array-of-tables API.

```bash
grep -n "fn.*table\|fn.*array\|fn.*get_str" userspace/procmgr/src/*.rs | head -30
```

If the TOML reader exposes `get_array_of_tables` or similar, use it. If not, you'll read via a string-parse helper already present. Adapt the code below to the API you find.

- [ ] **Step 2: Define `MountPolicy` enum and `MountPolicyEntry` struct**

Add near the top of `userspace/procmgr/src/main.rs` (after the other small enums — search for `enum CapProfile` or similar and add nearby):

```rust
/// Mount inheritance policy for a single path. Drives whether a nested
/// container's view inherits the parent's mount at that path or gets a
/// fresh backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MountPolicy {
    /// Use the parent container's mount entry verbatim (same MemFs).
    Inherit,
    /// Replace with a fresh per-container backend (current hardcoded behavior).
    Private,
    /// Inherit, but force writable=false. Deferred — may be unimplemented for now.
    Ro,
}

#[derive(Debug, Clone)]
struct MountPolicyEntry {
    path: String,
    policy: MountPolicy,
}

fn parse_mount_policy(s: &str) -> Option<MountPolicy> {
    match s {
        "inherit" => Some(MountPolicy::Inherit),
        "private" => Some(MountPolicy::Private),
        "ro" => Some(MountPolicy::Ro),
        _ => None,
    }
}
```

- [ ] **Step 3: Read `[[mounts.policy]]` in `handle_container_run`**

In `userspace/procmgr/src/main.rs` around line 4395 (right after the `deny_paths` read), add:

```rust
        // [[mounts.policy]] — per-path inheritance policy, applied on top of defaults.
        // Each entry: { path = "...", policy = "inherit"|"private"|"ro" }.
        let cluufile_mount_policies: Vec<MountPolicyEntry> = doc
            .array_of_tables("mounts.policy")
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| {
                        let path = t.get_str("path")?.to_string();
                        let policy_str = t.get_str("policy")?;
                        let policy = parse_mount_policy(policy_str)?;
                        Some(MountPolicyEntry { path, policy })
                    })
                    .collect()
            })
            .unwrap_or_default();
```

**If procmgr's TOML reader doesn't have `array_of_tables`**, write the minimum helper. In the same file as the TOML reader, add a method that iterates `[[mounts.policy]]` headers and returns a `Vec<SubTable>`. This is a one-off helper, so hardcode the header match if that's simpler:

```rust
// In the TOML reader (search for `impl Document` or similar):
pub fn array_of_tables(&self, name: &str) -> Option<Vec<&Table>> {
    // Return all tables whose header matches "[[name]]".
    // Implementation detail: depends on how the reader stores section headers.
    // See `doc.table(...)` for the single-table analogue.
    // ...
}
```

If that's nontrivial, fall back to a string scan over the raw manifest bytes inside `handle_container_run` — the manifest is small and scanning is fine:

```rust
        let cluufile_mount_policies: Vec<MountPolicyEntry> = parse_mount_policies_raw(&manifest_text);
```

...and implement `parse_mount_policies_raw` at module scope:

```rust
/// Minimal parser for [[mounts.policy]] array-of-tables in a manifest.toml.
/// Expects each entry to have `path = "..."` and `policy = "..."`.
fn parse_mount_policies_raw(manifest: &str) -> Vec<MountPolicyEntry> {
    let mut out = Vec::new();
    let mut in_section = false;
    let mut path: Option<String> = None;
    let mut policy: Option<MountPolicy> = None;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[[") {
            // Flush previous entry before entering a new section.
            if let (Some(p), Some(pol)) = (path.take(), policy.take()) {
                out.push(MountPolicyEntry { path: p, policy: pol });
            }
            in_section = trimmed == "[[mounts.policy]]";
            continue;
        }
        if trimmed.starts_with('[') {
            if let (Some(p), Some(pol)) = (path.take(), policy.take()) {
                out.push(MountPolicyEntry { path: p, policy: pol });
            }
            in_section = false;
            continue;
        }
        if !in_section { continue; }
        if let Some(rest) = trimmed.strip_prefix("path = ") {
            path = Some(rest.trim_matches('"').to_string());
        } else if let Some(rest) = trimmed.strip_prefix("policy = ") {
            policy = parse_mount_policy(rest.trim_matches('"'));
        }
    }
    if let (Some(p), Some(pol)) = (path, policy) {
        out.push(MountPolicyEntry { path: p, policy: pol });
    }
    out
}
```

- [ ] **Step 4: Add a minimal unit test for `parse_mount_policies_raw` (if used)**

If you wrote the raw-parse fallback, add at the bottom of `userspace/procmgr/src/main.rs`:

```rust
#[cfg(test)]
mod mount_policy_parse_tests {
    use super::*;

    #[test]
    fn parses_single_entry() {
        let m = "[[mounts.policy]]\npath = \"/tmp\"\npolicy = \"inherit\"\n";
        let out = parse_mount_policies_raw(m);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "/tmp");
        assert_eq!(out[0].policy, MountPolicy::Inherit);
    }

    #[test]
    fn parses_multiple_entries() {
        let m = "[[mounts.policy]]\npath = \"/tmp\"\npolicy = \"inherit\"\n\n[[mounts.policy]]\npath = \"/log\"\npolicy = \"private\"\n";
        let out = parse_mount_policies_raw(m);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].path, "/log");
        assert_eq!(out[1].policy, MountPolicy::Private);
    }

    #[test]
    fn ignores_other_sections() {
        let m = "[storage]\npersistent_dirs = [\"/data\"]\n\n[[mounts.policy]]\npath = \"/tmp\"\npolicy = \"private\"\n\n[exec]\nbinary = \"/bin/foo\"\n";
        let out = parse_mount_policies_raw(m);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "/tmp");
    }
}
```

Run: `cargo test -p procmgr mount_policy_parse_tests`
Expected: all pass.

**Note on no_std tests:** procmgr is `#![no_std]`. If `cargo test -p procmgr` doesn't work in isolation, either (a) add `#[cfg(all(test, feature = "std"))]` gating + a `std` feature, or (b) factor `parse_mount_policies_raw` into a small leaf file with `#![cfg_attr(not(test), no_std)]` and test that file. Prefer (b): add `userspace/procmgr/src/mount_policy.rs`, put the fn + tests there, and `use crate::mount_policy::...;` in main.rs.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs userspace/procmgr/src/mount_policy.rs 2>/dev/null
git commit -m "procmgr: parse [[mounts.policy]] entries from manifest.toml

Adds MountPolicy enum (Inherit/Private/Ro) and a minimal raw-text
parser for [[mounts.policy]] array-of-tables since procmgr's TOML
reader doesn't expose array-of-tables natively. Unit-tested against
representative manifests."
```

---

## Task 5: Extend VFS_SET_VIEW wire format with per-mount `memfs_cid`

**Files:**
- Modify: `userspace/procmgr/src/main.rs:5421-5452` (send_vfs_set_view)
- Modify: `userspace/procmgr/src/main.rs:62` (`type ViewMountList`)
- Modify: `userspace/vfs/src/main.rs:666-704` (VFS_SET_VIEW parser)
- Modify: `userspace/libcluu/src/ipc.rs` (comment/doc for VFS_SET_VIEW_LABEL payload layout)

- [ ] **Step 1: Change procmgr `ViewMountList` to carry memfs_cid**

In `userspace/procmgr/src/main.rs` around line 62, replace:

```rust
type ViewMountList = Vec<(String, String, bool)>;
```

With:

```rust
/// Per-mount entry sent to VFS: (src, dst, writable, memfs_cid).
/// `memfs_cid = 0` → mount resolves against the global MountTable (filesystem-backed).
/// `memfs_cid > 0` → mount resolves against that container's MemFs backend.
type ViewMountList = Vec<(String, String, bool, u64)>;
```

- [ ] **Step 2: Propagate the 4-tuple through all ViewMountList producers**

This is the mechanical part. Every place that currently constructs `(src, dst, writable)` tuples needs a fourth `u64` element (default `0` — preserves current MountTable behavior).

Search for all construction sites:

```bash
grep -n 'view_mounts\.push\|view_mounts\.insert\|mounts\.push.*String' userspace/procmgr/src/main.rs
```

Key locations to patch (fourth field = `0` unless noted):

- `default_view_for_profile` (grep for the fn) — append `0` to every tuple.
- `build_view_for_profile_and_home` (line ~457) — same.
- `apply_image_dir_overrides` (grep for fn) — same.
- `build_session_view` (line ~2031) — same.
- The caller-view filter at line 4556-4560 (`filter(|(_, dst, _)| ...)`) — update the destructure: `filter(|(_, dst, _, _)| ...)`.
- Likewise every `iter().position(|(_, dst, _)| ...)` → `iter().position(|(_, dst, _, _)| ...)`.
- The nested-container block at lines 4565-4575 — append `0` to each `push`.
- The top-level default block at lines 4577-4581 — no change; the fn being called is already patched.
- The hardcoded /tmp insert at lines 4587-4593 — will be fully replaced in Task 7; leave as-is for this commit, adding `0` for now: `view_mounts.insert(0, (format!(...), String::from("/tmp"), true, 0));`
- Persistent dirs at lines 4604-4608 — append `0`: `view_mounts.insert(0, (format!(...), format!(...), true, 0));`

This is busywork but each site is local; a `rustc` pass will point out any missed sites.

- [ ] **Step 3: Update procmgr wire serializer**

In `userspace/procmgr/src/main.rs` replace `send_vfs_set_view` (lines 5421-5452) with:

```rust
fn send_vfs_set_view(
    vfs_endpoint: usize,
    client_tid: usize,
    mounts: &[(String, String, bool, u64)],
    profile: CapProfile,
    container_id: u64,
) -> Result<()> {
    if vfs_endpoint == 0 {
        return Ok(());
    }

    // Wire format (per mount):
    //   u16 src_len LE | u16 dst_len LE | u8 flags | u64 memfs_cid LE |
    //   src_bytes       | dst_bytes
    //
    // flags: bit 0 = writable. `memfs_cid = 0` means MountTable; non-zero
    // means MountTarget::MemFs { container_id: memfs_cid }. This lets
    // procmgr express "mount /tmp against the PARENT container's MemFs"
    // for inherit policy.
    let mut payload = Vec::new();
    for (src, dst, writable, memfs_cid) in mounts {
        let src_bytes = src.as_bytes();
        let dst_bytes = dst.as_bytes();
        payload.extend_from_slice(&(src_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(&(dst_bytes.len() as u16).to_le_bytes());
        payload.push(if *writable { 1u8 } else { 0u8 });
        payload.extend_from_slice(&memfs_cid.to_le_bytes());
        payload.extend_from_slice(src_bytes);
        payload.extend_from_slice(dst_bytes);
    }

    let mut msg = Message::new(ipc::VFS_SET_VIEW_LABEL, [0; 6], 5);
    msg.words[0] = payload.len();
    msg.words[1] = client_tid;
    msg.words[2] = mounts.len();
    msg.words[3] = profile.bits() as usize;
    msg.words[4] = container_id as usize;
    ipc::send_msg_with_payload(vfs_endpoint, &msg, &payload)
}
```

- [ ] **Step 4: Update VFS wire parser**

In `userspace/vfs/src/main.rs:666-704` replace the per-mount parse loop with:

```rust
        let mut mounts = alloc::vec::Vec::new();
        let mut offset = 0;

        for _ in 0..mount_count {
            // Per-mount wire format:
            //   u16 src_len LE | u16 dst_len LE | u8 flags | u64 memfs_cid LE | src | dst
            // Header size: 2 + 2 + 1 + 8 = 13 bytes.
            if offset + 13 > payload.len() {
                return Err(Error::InvalidArgument);
            }
            let src_len = u16::from_le_bytes([payload[offset], payload[offset + 1]]) as usize;
            let dst_len = u16::from_le_bytes([payload[offset + 2], payload[offset + 3]]) as usize;
            let flags = payload[offset + 4];
            let memfs_cid = u64::from_le_bytes([
                payload[offset + 5],
                payload[offset + 6],
                payload[offset + 7],
                payload[offset + 8],
                payload[offset + 9],
                payload[offset + 10],
                payload[offset + 11],
                payload[offset + 12],
            ]);
            offset += 13;

            if offset + src_len + dst_len > payload.len() {
                return Err(Error::InvalidArgument);
            }
            let src: alloc::string::String =
                core::str::from_utf8(&payload[offset..offset + src_len])
                    .map_err(|_| Error::InvalidArgument)?
                    .into();
            offset += src_len;
            let dst: alloc::string::String =
                core::str::from_utf8(&payload[offset..offset + dst_len])
                    .map_err(|_| Error::InvalidArgument)?
                    .into();
            offset += dst_len;
            view::validate_clean_absolute_path(src.as_str())?;
            view::validate_clean_absolute_path(dst.as_str())?;

            let target = if memfs_cid == 0 {
                view::MountTarget::MountTable
            } else {
                // Lazily allocate the MemFs for this container on first sight.
                if !self.container_memfs.contains_key(&memfs_cid) {
                    let memfs = mount::MemFsBackend::new(DEFAULT_MEMFS_QUOTA);
                    {
                        let mut fs = memfs.borrow_mut();
                        let _ = fs.mkdir("/tmp");
                        let _ = fs.mkdir("/log");
                    }
                    self.container_memfs.insert(memfs_cid, memfs);
                }
                view::MountTarget::MemFs { container_id: memfs_cid }
            };

            mounts.push(view::ViewMount {
                src,
                dst,
                writable: (flags & 1) != 0,
                target,
            });
        }
        if offset != payload.len() {
            return Err(Error::InvalidArgument);
        }
```

**Do not yet remove** the unconditional container-isolation block at lines 706-759. That's Task 7. Leaving it in place means this commit preserves current behavior — wire format accepts `memfs_cid` but procmgr still sends `0`, so the VFS-side prepend still runs and the outcome is identical.

- [ ] **Step 5: Update the doc comment on `VFS_SET_VIEW_LABEL`**

In `userspace/libcluu/src/ipc.rs` near line 69 (`pub const VFS_SET_VIEW_LABEL: u32 = 21;`), add or update the doc comment:

```rust
/// Set the per-client VFS view (mount list). Request from procmgr to VFS.
///
/// Message words:
///   [0] payload length in bytes
///   [1] target client_tid (0 = sender_tid)
///   [2] mount count
///   [3] CapProfile bits
///   [4] container_id (u64 fits in usize on x86_64)
///
/// Per-mount wire layout:
///   u16 src_len LE | u16 dst_len LE | u8 flags | u64 memfs_cid LE |
///   src_bytes (src_len) | dst_bytes (dst_len)
///
/// Flags bit 0 = writable. `memfs_cid = 0` resolves the mount against the
/// global MountTable; `memfs_cid > 0` resolves against that container's
/// per-container MemFs backend (procmgr owns the keying).
pub const VFS_SET_VIEW_LABEL: u32 = 21;
```

- [ ] **Step 6: Build to verify compile**

Run: `cargo xtask build`
Expected: successful build. If `rustc` reports pattern-match sites you missed (e.g., `expected a 3-tuple`), patch them by appending `_` or `0` as appropriate.

- [ ] **Step 7: Smoke-test with the existing `m1_recv` harness**

Run: `MARKER_MODE=m1_recv bash scripts/harness_run.sh`
Expected: PASS. Behavior is unchanged because procmgr still sends `memfs_cid = 0` for everything and VFS's unconditional prepend still drives container isolation.

If this fails, the wire-format change broke something. Debug before moving on.

- [ ] **Step 8: Commit**

```bash
git add userspace/procmgr/src/main.rs userspace/vfs/src/main.rs userspace/libcluu/src/ipc.rs
git commit -m "vfs: extend VFS_SET_VIEW wire format with per-mount memfs_cid

Adds a u64 memfs_cid field to each mount entry in the VFS_SET_VIEW
payload. memfs_cid = 0 resolves against the global MountTable (today's
default); memfs_cid > 0 resolves against that container's per-container
MemFs backend. VFS lazily allocates MemFs on first sighting of a given
memfs_cid.

Behavior-preserving: procmgr still sends 0 for all mounts, and the VFS-
side unconditional container-isolation prepend still fires. Task 7 will
flip procmgr to send explicit memfs_cid values and drop the VFS prepend."
```

---

## Task 6: Policy resolution — default table + Cluufile overrides

**Files:**
- Modify: `userspace/procmgr/src/main.rs` (new module: `mount_policy.rs` or inlined)

- [ ] **Step 1: Add failing unit tests**

In the `mount_policy` submodule (or wherever you placed `parse_mount_policies_raw`), add:

```rust
#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn ep(path: &str, policy: MountPolicy) -> MountPolicyEntry {
        MountPolicyEntry { path: path.to_string(), policy }
    }

    #[test]
    fn defaults_applied_when_no_cluufile_entries() {
        let resolved = resolve_effective_policies(&[], false);
        // /tmp defaults to Inherit, /log to Private.
        assert_eq!(lookup(&resolved, "/tmp"), Some(MountPolicy::Inherit));
        assert_eq!(lookup(&resolved, "/log"), Some(MountPolicy::Private));
    }

    #[test]
    fn cluufile_override_wins() {
        let custom = vec![ep("/tmp", MountPolicy::Private)];
        let resolved = resolve_effective_policies(&custom, false);
        assert_eq!(lookup(&resolved, "/tmp"), Some(MountPolicy::Private));
        // /log default still applies.
        assert_eq!(lookup(&resolved, "/log"), Some(MountPolicy::Private));
    }

    #[test]
    fn deny_inherit_yields_empty_policy_set() {
        let custom = vec![ep("/tmp", MountPolicy::Inherit)];
        let resolved = resolve_effective_policies(&custom, true);
        // DENY_INHERIT means no inheritance at all — MOUNT entries are ignored.
        assert!(resolved.is_empty());
    }

    fn lookup(policies: &[MountPolicyEntry], path: &str) -> Option<MountPolicy> {
        policies.iter().find(|e| e.path == path).map(|e| e.policy)
    }
}
```

- [ ] **Step 2: Verify they fail**

Run: `cargo test -p procmgr resolve_tests` (or whichever crate holds the module).
Expected: compile error — `resolve_effective_policies` doesn't exist.

- [ ] **Step 3: Implement `resolve_effective_policies`**

In the same module:

```rust
/// Default mount policy table. Paths not listed here get no entry (meaning
/// the view-inheritance code path applies without per-path fiddling).
///
/// - `/tmp → Inherit`: shell session anchor; child processes see shell's /tmp.
///   Containers that want isolation opt in via `MOUNT /tmp private`.
/// - `/log → Private`: per-container log scope is the whole point of /log.
///
/// Other paths like /data are handled via the PERSISTENT directive upstream
/// and do not appear in this table.
fn default_mount_policies() -> [(&'static str, MountPolicy); 2] {
    [
        ("/tmp", MountPolicy::Inherit),
        ("/log", MountPolicy::Private),
    ]
}

/// Compose defaults + Cluufile overrides into a single effective policy list.
/// Cluufile entries win over defaults on the same path. When `deny_inherit`
/// is set, returns an empty list because there's nothing to inherit — the
/// DENY_INHERIT code path already produces a fresh image-only view.
pub fn resolve_effective_policies(
    cluufile_entries: &[MountPolicyEntry],
    deny_inherit: bool,
) -> Vec<MountPolicyEntry> {
    if deny_inherit {
        return Vec::new();
    }
    let mut out: Vec<MountPolicyEntry> = Vec::new();
    // Seed with defaults.
    for (path, policy) in default_mount_policies().iter() {
        out.push(MountPolicyEntry { path: path.to_string(), policy: *policy });
    }
    // Apply Cluufile overrides.
    for entry in cluufile_entries {
        if let Some(existing) = out.iter_mut().find(|e| e.path == entry.path) {
            existing.policy = entry.policy;
        } else {
            out.push(entry.clone());
        }
    }
    out
}
```

- [ ] **Step 4: Run tests to confirm pass**

Run: `cargo test -p procmgr resolve_tests mount_policy_parse_tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs userspace/procmgr/src/mount_policy.rs 2>/dev/null
git commit -m "procmgr: resolve effective mount policies (defaults + Cluufile)

Defaults: /tmp → inherit (flip from today's hardcoded private), /log →
private (unchanged). Cluufile [[mounts.policy]] entries override the
default for the listed path. DENY_INHERIT short-circuits policy
resolution to empty since there's nothing to inherit from.

This is pure logic — not yet wired into the view builder."
```

---

## Task 7: procmgr — apply policy in view builder; VFS — drop unconditional prepend

This is the atomic behavior flip. After this task, `/tmp` inheritance actually works.

> **Implementation deviation (commit e1887f5):** Step 2's single
> `container_system_mounts` helper was split into two helpers —
> `container_system_mounts` returns only `/data` (PREPENDED) and a new
> `container_catchall_mount` returns only `/` (APPENDED). Reason: VFS's
> `view::resolve_path` is first-match-wins (see `userspace/vfs/src/view.rs`
> around the mount-iteration loop); a prepended `/ → /` mount would
> shadow every other mount in the list. The append-catchall pattern
> matches what the deleted VFS block used to do
> (`mounts.extend([container, caller]).push("/")`).

**Files:**
- Modify: `userspace/procmgr/src/main.rs:4549-4612` (the view-building block inside `handle_container_run`)
- Modify: `userspace/vfs/src/main.rs:706-759` (drop the unconditional container-isolation prepend)

- [ ] **Step 1: Drop VFS unconditional container prepend**

In `userspace/vfs/src/main.rs` replace lines 706-759 (from `// Container isolation: create private dirs ...` through the end of the `if container_id > 0 { ... }` block — including the catch-all `/` prepend and the `mounts = all_mounts;` line) with:

```rust
        // Record container membership for later cleanup/ringio paths.
        if container_id > 0 {
            self.client_containers.insert(client_id, container_id);
        }
        // NOTE: /tmp, /log, /data, and the `/ → MemFs` catch-all are now
        // procmgr's responsibility. procmgr sends explicit mount entries
        // with the correct memfs_cid per mount (see mount-policy design
        // spec). VFS just serves whatever mount list it's given.
```

Keep everything after (the `debug_print` and `self.views.set_view(...)` block) unchanged.

- [ ] **Step 2: Add `/`, `/data` (PERSISTENT), and MemFs defaults builder in procmgr**

In `userspace/procmgr/src/main.rs` add a helper function near the other view helpers (search for `fn default_view_for_profile` and add alongside):

```rust
/// Build the per-container system mounts that were previously created inside
/// VFS's set_view. These are path-specific entries (not passthrough) that
/// define the shape of the ephemeral storage:
///   /data  — MountTable-backed, path = /var/containers/c-<cid>/data, writable
///   /      — catch-all MemFs { own_cid }, writable (so readdir("/") and
///            reads of non-covered paths don't ENOENT)
/// /tmp and /log are added separately because their memfs_cid depends on
/// the resolved mount policy for this container.
fn container_system_mounts(container_id: u64) -> ViewMountList {
    if container_id == 0 {
        return Vec::new();
    }
    alloc::vec![
        (
            format!("/var/containers/c-{}/data", container_id),
            String::from("/data"),
            true,
            0, // MountTable — persistent/ext2-backed via MountTable
        ),
        (
            String::from("/"),
            String::from("/"),
            true,
            container_id, // catch-all resolves to own MemFs
        ),
    ]
}

/// Build /tmp and /log mounts for this container given the resolved policy.
/// - Private or no parent → memfs_cid = own container_id (fresh MemFs)
/// - Inherit with parent  → memfs_cid = caller_container_id (parent's MemFs)
/// - Ro                   → same as Inherit but writable=false (stretch goal)
fn policy_driven_memfs_mounts(
    policies: &[MountPolicyEntry],
    own_cid: u64,
    parent_cid: u64,
) -> ViewMountList {
    let mut out = ViewMountList::new();
    for entry in policies {
        // Only /tmp and /log are MemFs-backed today. If the user declares a
        // MOUNT on some other path, it has no effect here (we fall through
        // to view passthrough, which already inherits by default).
        if entry.path != "/tmp" && entry.path != "/log" {
            continue;
        }
        let (cid, writable) = match entry.policy {
            MountPolicy::Inherit if parent_cid != 0 => (parent_cid, true),
            MountPolicy::Inherit => (own_cid, true), // top-level: no parent to inherit from
            MountPolicy::Private => (own_cid, true),
            MountPolicy::Ro if parent_cid != 0 => (parent_cid, false),
            MountPolicy::Ro => (own_cid, false),
        };
        out.push((entry.path.clone(), entry.path.clone(), writable, cid));
    }
    out
}
```

- [ ] **Step 3: Replace the hardcoded `/tmp` swap block in `handle_container_run`**

In `userspace/procmgr/src/main.rs` replace lines 4583-4612 (the block starting `// Container-scoped /tmp:` through the end of the persistent-dirs handling — stop just before `self.pid_to_container_id.insert(pid, container_id);`) with:

```rust
                // Resolve effective mount policies (defaults + Cluufile overrides).
                let effective_policies = mount_policy::resolve_effective_policies(
                    &cluufile_mount_policies,
                    deny_inherit,
                );

                // Strip any /tmp, /log, /data, or / mounts inherited from the
                // caller view — procmgr owns those paths for this container.
                let container_anchored = ["/tmp", "/log", "/data", "/"];
                view_mounts.retain(|(_, dst, _, _)| {
                    !container_anchored.iter().any(|a| dst == *a)
                });

                // Prepend policy-driven /tmp and /log mounts with the right
                // memfs_cid (first-match-wins — these shadow any leftover
                // passthrough entries that slipped through retain above).
                let memfs_mounts = policy_driven_memfs_mounts(
                    &effective_policies,
                    container_id,
                    caller_container_id,
                );
                for m in memfs_mounts.into_iter().rev() {
                    view_mounts.insert(0, m);
                }

                // /data and the / catch-all are per-container system mounts
                // regardless of policy.
                for m in container_system_mounts(container_id).into_iter().rev() {
                    view_mounts.insert(0, m);
                }

                // PERSISTENT directives already contribute to view_mounts via
                // the existing storage-table loop below (preserve that path).
                if has_persistent_storage && container_id > 0 {
                    if let Some(storage_table) = doc.table("storage") {
                        if let Some(pdirs) = storage_table.get_array("persistent_dirs") {
                            for pdir in pdirs {
                                let dir_name = pdir.trim_start_matches('/');
                                if let Some(pos) = view_mounts.iter().position(|(_, dst, _, _)| dst.trim_start_matches('/') == dir_name) {
                                    view_mounts.remove(pos);
                                }
                                view_mounts.insert(0, (
                                    format!("/var/containers/c-{}/{}", container_id, dir_name),
                                    format!("/{}", dir_name),
                                    true,
                                    0, // MountTable-backed (ext2)
                                ));
                            }
                        }
                    }
                }
```

If your module organization calls the policy helpers `crate::mount_policy::resolve_effective_policies` etc., import them with a `use crate::mount_policy::...` at the top.

- [ ] **Step 4: Build the system**

Run: `cargo xtask build`
Expected: clean build. Fix any remaining compile errors (likely pattern-match arity or missing imports).

- [ ] **Step 5: Run `m1_recv` smoke**

Run: `MARKER_MODE=m1_recv bash scripts/harness_run.sh`
Expected: PASS. Basic shell + container flow must still work.

- [ ] **Step 6: Run existing `l2_mkdir` harness case**

Run: `MARKER_MODE=l2_mkdir bash scripts/harness_run.sh`
Expected: PASS. `/bin/mkdir` already works for single-spawn uses; this confirms /tmp still exists and is writable from the first spawn.

- [ ] **Step 7: Run `l2_rm` harness case — the previously-failing one**

Run: `MARKER_MODE=l2_rm bash scripts/harness_run.sh`
Expected: PASS. The multi-spawn sequence (`spawn mkdir /tmp/rmtest; spawn mkdir /tmp/rmtest/inner; spawn rm -r /tmp/rmtest`) now succeeds because `/tmp` inherits across spawns under the shell session.

If this still fails, debug: check serial log for `vfs: set_view` entries for each spawn, verify their mount lists, verify the `memfs_cid` values. The second spawn's `/tmp` mount should carry the shell's container_id, not the spawn's own.

- [ ] **Step 8: Commit**

```bash
git add userspace/procmgr/src/main.rs userspace/vfs/src/main.rs
git commit -m "procmgr/vfs: policy-driven /tmp inheritance for nested containers

Replaces the unconditional per-container /tmp swap (procmgr:4583 and
vfs:706) with policy-driven mount construction. Defaults apply:
/tmp → inherit, /log → private, catch-all / → own MemFs. Cluufile
[[mounts.policy]] entries override the default per path.

Nested spawns now share their shell's /tmp MemFs (memfs_cid = parent's
container_id). Shell isolates its own /tmp from init via the explicit
MOUNT /tmp private directive added in a follow-up commit.

Unblocks Shell-A Plan 2: l2_rm now passes end-to-end."
```

---

## Task 8: Shell Cluufile — explicit `MOUNT /tmp private` session anchor

**Files:**
- Modify: `containers/shell/Cluufile`

- [ ] **Step 1: Add `MOUNT /tmp private` to shell Cluufile**

Edit `containers/shell/Cluufile` to add a `MOUNT` line. After the change:

```
FROM minimal
PROFILE ipc spawn registry vfs
MOUNT /tmp private
BUILD "cargo build --manifest-path userspace/shell/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/shell.elf /bin/shell
ENTRYPOINT /bin/shell
```

This establishes the shell as the root of its own `/tmp` scope: shell's `/tmp` is a fresh MemFs (not inherited from init's unused top-level `/tmp`), and every container the shell spawns inherits this per-shell `/tmp`.

- [ ] **Step 2: Rebuild shell container**

Run: `cargo xtask build`
Expected: clean build. The `container-build` tool parses the new directive and emits `[[mounts.policy]]\npath = "/tmp"\npolicy = "private"\n` into `target/containers/shell/manifest.toml`.

Verify:

```bash
grep -A2 'mounts.policy' target/containers/shell/manifest.toml
```

Expected output includes:
```
[[mounts.policy]]
path = "/tmp"
policy = "private"
```

- [ ] **Step 3: Re-run `l2_rm` and `l2_mkdir`**

```bash
MARKER_MODE=l2_mkdir bash scripts/harness_run.sh
MARKER_MODE=l2_rm    bash scripts/harness_run.sh
```

Expected: both PASS.

Without the shell's explicit `MOUNT /tmp private`, shell would inherit init's `/tmp` by default — harmless because init's `/tmp` is unused, but implicit. With the explicit private, the session-scope boundary is explicit and future-proof.

- [ ] **Step 4: Commit**

```bash
git add containers/shell/Cluufile
git commit -m "shell: declare explicit MOUNT /tmp private session anchor

Under the new default policy /tmp → inherit, shell would inherit init's
/tmp implicitly. Init's /tmp is unused so the practical behavior is the
same as today, but 'implicit /tmp scope' is a foot-gun. Declaring
MOUNT /tmp private makes the shell's session-/tmp boundary explicit —
shell gets its own fresh MemFs and every nested spawn inherits it."
```

---

## Task 9: Harness case `l2_mount_private` — verify isolation

**Files:**
- Create: `containers/mountprobe/Cluufile`
- Create: `containers/mountprobe/Cargo.toml`
- Create: `userspace/mountprobe/src/main.rs`
- Create: `userspace/mountprobe/Cargo.toml`
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create the probe binary**

Create `userspace/mountprobe/Cargo.toml`:

```toml
[package]
name = "mountprobe"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "mountprobe"
path = "src/main.rs"

[dependencies]
libcluu = { path = "../libcluu", features = ["posix"] }

[profile.dev]
panic = "abort"
[profile.release]
panic = "abort"
```

Create `userspace/mountprobe/src/main.rs`:

```rust
//! Harness helper for l2_mount_private: verifies that a container declared
//! with MOUNT /tmp private does NOT see files the caller placed in /tmp,
//! because its /tmp resolves to a fresh per-container MemFs.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::fs::client::VfsClient;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let vfs = match VfsClient::connect() {
        Ok(c) => c,
        Err(e) => {
            let _ = libcluu::debug_print(&format!("mountprobe: FAIL vfs connect {:?}", e));
            return 1;
        }
    };

    // The harness script pre-creates /tmp/MOUNTPROBE_CANARY in the shell's
    // /tmp before spawning us. Since our Cluufile declares MOUNT /tmp private,
    // we should see an empty /tmp (only the mkdir'd /tmp dir, no canary).
    match vfs.stat("/tmp/MOUNTPROBE_CANARY") {
        Ok(_) => {
            let _ = libcluu::debug_print("mountprobe: FAIL canary visible in private /tmp");
            1
        }
        Err(_) => {
            let _ = libcluu::debug_print("mountprobe: PASS /tmp isolation verified");
            0
        }
    }
}
```

- [ ] **Step 2: Create the container Cluufile**

Create `containers/mountprobe/Cluufile`:

```
FROM minimal
PROFILE ipc vfs registry
MOUNT /tmp private
BUILD "cargo build --manifest-path userspace/mountprobe/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/mountprobe.elf /bin/mountprobe
ENTRYPOINT /bin/mountprobe
```

- [ ] **Step 3: Register the workspace member**

Check the workspace Cargo.toml:

```bash
grep -n 'mountprobe\|userspace/rm\|userspace/mkdir' Cargo.toml
```

Expected: `userspace/rm` and `userspace/mkdir` already present as `members = [...]` entries. Add `"userspace/mountprobe"` alongside them (alphabetically by convention).

If your workspace uses an `exclude` list plus auto-discovery, confirm `userspace/mountprobe` isn't excluded.

- [ ] **Step 4: Register the harness case**

Edit `scripts/harness_cases.conf`. Insert a new line alphabetically within the `l2_*` block (between `l2_mkdir` and `l2_owner_deny`, or between `l2_jobmix` and `l2_mkdir` — doesn't really matter, prefer alphabetical):

```
l2_mount_private|full|MARKER_MODE=l2_mount_private TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

- [ ] **Step 5: Register the autostart command and markers**

Edit `scripts/harness_case_defaults.sh` — add inside the `case "$MARKER_MODE" in` block (near the other `l2_*` cases, e.g. after `l2_mkdir`):

```sh
            l2_mount_private)
                TEST_COMMAND=""
                # Seed shell's /tmp, then spawn the probe. The probe should see an
                # empty /tmp because its Cluufile declares MOUNT /tmp private.
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mkdir /tmp; touch /tmp/MOUNTPROBE_CANARY; spawn mountprobe"
                ;;
```

**Note on `touch`:** The shell may or may not have a `touch` builtin. If it doesn't, use `spawn mkdir /tmp/MOUNTPROBE_CANARY` as a proxy (a visible directory in /tmp counts just as well for the isolation check). Adjust the probe's `stat` call to match: if the canary is a directory, `stat` on it still returns Ok and the probe logic still holds.

If `touch` is absent, prefer the directory proxy — simpler and closer to what the other L2 tests do. Rewrite the autostart as:

```sh
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mkdir /tmp/MOUNTPROBE_CANARY; spawn mountprobe"
```

Edit `scripts/harness_run.sh` — add inside the `case "$MARKER_MODE" in` required-markers block (near `l2_mkdir`):

```sh
    l2_mount_private)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "mountprobe: PASS /tmp isolation verified"
        )
        ;;
```

- [ ] **Step 6: Build and run the new case**

```bash
cargo xtask build
MARKER_MODE=l2_mount_private bash scripts/harness_run.sh
```

Expected: PASS. The probe reports "PASS /tmp isolation verified" because the shell's `/tmp/MOUNTPROBE_CANARY` is invisible to `mountprobe`'s fresh private `/tmp`.

- [ ] **Step 7: Negative control — sanity-check the default inherit path**

This is a one-off diagnostic, no commit. Temporarily remove the `MOUNT /tmp private` line from `containers/mountprobe/Cluufile`, rebuild, re-run:

```bash
cargo xtask build
MARKER_MODE=l2_mount_private bash scripts/harness_run.sh
```

Expected: FAIL with "mountprobe: FAIL canary visible in private /tmp". This confirms the canary IS visible under the default inherit policy — proving the test actually distinguishes the two behaviors.

Now restore the `MOUNT /tmp private` line, rebuild, re-run to green.

- [ ] **Step 8: Commit**

```bash
git add userspace/mountprobe containers/mountprobe scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh Cargo.toml
git commit -m "harness: l2_mount_private verifies MOUNT /tmp private isolation

New probe container 'mountprobe' declares MOUNT /tmp private in its
Cluufile. The harness shell seeds a canary directory in its /tmp, then
spawns mountprobe. The probe stats the canary path and PASSes when the
stat fails (canary invisible = isolation verified).

Negative-controlled by temporarily removing the MOUNT directive during
development and observing the canary become visible (default inherit)."
```

---

## Task 10: Re-verify blocked Shell-A Plan 2 harness cases

**Files:**
- No code changes — just a verification gate.

- [ ] **Step 1: Re-run `l2_rm`**

```bash
MARKER_MODE=l2_rm bash scripts/harness_run.sh
```

Expected: PASS. Confirms the original Shell-A Plan 2 blocker is gone.

- [ ] **Step 2: Re-run `l2_mkdir`**

```bash
MARKER_MODE=l2_mkdir bash scripts/harness_run.sh
```

Expected: PASS.

- [ ] **Step 3: Re-run the previously-flaky `l2_owner_deny`**

```bash
MARKER_MODE=l2_owner_deny bash scripts/harness_run.sh
```

Expected: either reliably PASS or reliably FAIL (no flake). Per memory
(`project_l2_owner_deny_flaky`), this test's flake was rooted in the
same cross-container /tmp race. If it now fails reliably, open a
follow-up task; the spec §Risks calls this out as a distinct issue.

- [ ] **Step 4: No commit — verification only.**

---

## Task 11: Full harness matrix regression

**Files:**
- No code changes — final gate.

- [ ] **Step 1: Run the full harness suite**

```bash
bash scripts/harness_suite.sh
```

Expected: every case passes. Pay special attention to:
- `m1_recv`, `m2_*`, `m3_*`, `m4_*`, `m5_fairness` — core IPC / leak diagnostics.
- `e13_container_run`, `f8_nested_container_run`, `f10_view_passthrough`,
  `f11_deny_inherit`, `f12_cascade_cleanup`, `f13_detach_survive` —
  container-lifecycle cases that exercise the view-inheritance machinery.
- `l2_mkdir`, `l2_rm`, `l2_mount_private` — the direct targets.
- `l2_argv`, `l2_cd`, `l2_cd_inherit` — Shell-A Plan 1 regressions.

- [ ] **Step 2: If any regression surfaces, diagnose before closing**

Common failure shapes:
- "fresh /tmp" expectation in a probe → needs `MOUNT /tmp private` in that probe's Cluufile.
- ViewMount arity mismatch → pattern-match site missed in Task 5 Step 2.
- MemFs backend allocation → look for memfs_cid mismatches in debug_print output.

- [ ] **Step 3: Record matrix result and commit nothing.**

The matrix is the final gate; a green run is the sign-off. No code commit needed.

- [ ] **Step 4: Update memory**

Per `feedback_always_commit_plans` memory, commit any related state, and add a one-line entry to `MEMORY.md`:

```
- [Mount policy replaces hardcoded /tmp swap](project_mount_policy.md) — /tmp → inherit by default; shell anchors session scope via MOUNT /tmp private; Cluufile [[mounts.policy]] is the override surface.
```

Create the memory file at `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_mount_policy.md`:

```markdown
---
name: Mount policy replaces hardcoded /tmp swap
description: Cluufile MOUNT directive + per-mount memfs_cid wire format; default /tmp → inherit; shell Cluufile anchors session scope
type: project
---

Shell-A Plan 2 blocker (l2_rm) was that nested container spawns each got
a fresh per-container /tmp MemFs, breaking `spawn mkdir /tmp/x; spawn rm
-r /tmp/x` sequences. Resolved 2026-04-23 by Cluufile `MOUNT <path>
<policy>` directive; per-mount `memfs_cid` in VFS_SET_VIEW wire format
lets procmgr express "this /tmp uses parent's MemFs".

**Defaults:** /tmp → inherit (flipped), /log → private (unchanged).
**Shell anchor:** containers/shell/Cluufile declares `MOUNT /tmp private`
to establish session-scoped /tmp.

**Why:** Shell-pipe ergonomics require /tmp continuity across spawns;
per-spawn isolation was the wrong default. Containers wanting isolation
opt in via `MOUNT /tmp private`.

**How to apply:** When diagnosing "probe doesn't see files I wrote to
/tmp", check the probe's Cluufile — an explicit `MOUNT /tmp private`
means isolation is intentional. When adding a new probe that needs
visibility into shell's /tmp, omit the MOUNT directive (default inherit).
```

Commit:

```bash
git add /home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/
git commit -m "memory: record mount-policy project state"
```

---

## Self-Review Results

Running the skill's self-review checklist inline:

**1. Spec coverage**

| Spec section | Plan task |
|---|---|
| New Cluufile `MOUNT <path> <policy>` directive | Task 1 |
| Policy values inherit/private/ro | Task 1 (ro parses; Task 7 leaves ro as "stretch") |
| Parse validation (absolute path, known policy, no duplicates) | Task 1 |
| Default policy table (`/tmp → inherit`, `/log → private`) | Task 6 |
| Interaction with `DENY_INHERIT` (policies ignored) | Task 6 (`deny_inherit` short-circuit) |
| Interaction with `DENY` (error at parse time) | Task 3 |
| Interaction with `PERSISTENT` (error at parse time) | Task 3 |
| Nesting semantics (`caller_container_id != 0`) | Task 7 (`parent_cid` logic) |
| Shell session anchor | Task 8 |
| Cluufile parser implementation surface | Tasks 1–3 |
| Manifest `[[mounts.policy]]` table | Task 2 |
| Procmgr consumption of policy | Tasks 4, 6, 7 |
| VFS side (the spec said "no change" — WRONG) | Task 5 Step 4 + Task 7 Step 1 |
| Test plan: regression + Plan 2 unblock + l2_mount_private | Tasks 9, 10, 11 |

All spec sections covered. The VFS side — which the spec mis-identified as "no change" — is flagged in the header note and handled across Tasks 5 and 7.

**2. Placeholder scan**

No "TBD", "TODO", "implement later" markers in the plan. Every step shows concrete code or an exact shell invocation. The procmgr TOML-reader contingency in Task 4 Step 3 has two labeled alternatives — both concrete — rather than a "figure it out" placeholder.

**3. Type consistency**

- `MountPolicy` (enum) + `MountPolicyEntry` (struct) defined Task 4 Step 2, used Task 4 Step 3, Task 6, Task 7.
- `ViewMountList = Vec<(String, String, bool, u64)>` (Task 5 Step 1) — all construction and consumption sites updated with the 4-tuple shape in Task 5 Step 2.
- `resolve_effective_policies` signature: `(&[MountPolicyEntry], bool) -> Vec<MountPolicyEntry>` — defined Task 6 Step 3, used Task 7 Step 3.
- `policy_driven_memfs_mounts` signature: `(&[MountPolicyEntry], u64, u64) -> ViewMountList` — defined Task 7 Step 2, used Task 7 Step 3.
- `container_system_mounts(u64) -> ViewMountList` — defined Task 7 Step 2, used Task 7 Step 3.
- Wire format: u16 | u16 | u8 | u64 | src | dst — 13-byte header in both serializer (Task 5 Step 3) and parser (Task 5 Step 4).

All names and signatures match across tasks.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-23-mount-policy.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — dispatch fresh subagent per task with two-stage review (spec compliance + code quality) between tasks. Fast iteration, context-isolated per task.

**2. Inline Execution** — execute tasks in this session using `superpowers:executing-plans`, batched with checkpoints for review.

**Which approach?**
