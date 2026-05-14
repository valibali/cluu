# Plan 2: Envelope {vt}/{user} substitution + vt_text/vt_graphical profiles

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `/etc/envelopes.toml` carry per-shape mount lists (`vt_text` vs `vt_graphical`) and apply both `{vt}` and `{user}` substitutions at SESSION_LOGIN, so each session sees the strict subset of `/dev` (and elsewhere) defined by its envelope and VT index. Closes HOME-not-propagating (Bug B) by giving root a real `env_template`, plus introduces per-VT `/dev/tty<N>` narrowing required by the unified `/dev` model.

**Architecture:** `userspace/procmgr/src/envelopes.rs` already does `{user}` substitution via `resolve_env`. Extend the same module to also substitute `{vt}` in mount paths, and to look up the right sub-profile (`vt_text` vs `vt_graphical`) based on `session_kind`. Procmgr passes the selected profile to `build_view_from_envelope`. The view layer (`vfs_view.rs`) already enforces monotone narrowing — this plan adds an explicit audit task to confirm substitution doesn't slip past that check.

**Tech Stack:** Rust, TOML parsing (existing `toml` workspace dep), CLUU envelope code, harness for end-to-end verification.

**Depends on:** Plan 1 (procmgr opens /dev/tty<N> via FDAC — needs the substituted mount visible to the session).

---

## Pre-flight context

Files touched:

| File | Action |
|---|---|
| `userspace/procmgr/src/envelopes.rs` | Extend `Envelope` struct with optional `vt_text` / `vt_graphical` mount lists; add `{vt}` substitution to `resolve_env` (rename to `resolve_session_env`); add unit tests |
| `userspace/procmgr/src/main.rs` | Call the new resolver with `session_kind` + `vt` at SESSION_LOGIN; update `build_view_from_envelope` to consume the selected mounts |
| `etc/envelopes.toml` | Add `vt_text` / `vt_graphical` sub-tables for `user` and `admin`; give root a real env_template (currently empty, the root-cause of Bug B) |
| `scripts/harness_case_defaults.sh` + `scripts/harness_run.sh` | Two new markers: `l2_envelope_dev_filter` (VT0 user sees only `tty0`) and `l2_envelope_home_propagated` (HOME=/home/root visible to shell) |

Existing helpers to reuse:
- `resolve_env(envelope, user)` at `userspace/procmgr/src/envelopes.rs:38`.
- `parse_envelopes(toml_str)` at `userspace/procmgr/src/envelopes.rs:51`.
- `build_view_from_envelope(envelope)` in `userspace/procmgr/src/main.rs` — locate via `grep -n build_view_from_envelope`.

Test data shape (unit tests live alongside code in `envelopes.rs`):

```toml
[envelope.user.env]
SHELL = "/bin/shell"
[envelope.user.env_template]
HOME = "/home/{user}"
[envelope.user.vt_text.mounts]
list = ["ro:/dev/tty{vt}", "ro:/dev/null"]
[envelope.user.vt_graphical.mounts]
list = ["rw:/dev/pts", "rw:/dev/fb0"]
```

---

## Task 1: Failing unit test for {vt} substitution

**Files:**
- Modify: `userspace/procmgr/src/envelopes.rs:130-180` (existing tests block).

- [ ] **Step 1: Add a failing test**

Append below the existing tests (locate `#[cfg(test)] mod tests` block around line 110):

```rust
    #[test]
    fn vt_substitution_in_mount_paths() {
        let toml_input = r#"
[envelope.user]
[envelope.user.env]
SHELL = "/bin/shell"
[envelope.user.env_template]
HOME = "/home/{user}"
[envelope.user.vt_text]
mounts = ["ro:/dev/tty{vt}", "ro:/dev/null"]
[envelope.user.vt_graphical]
mounts = ["rw:/dev/pts", "rw:/dev/fb0"]
"#;
        let envs = parse_envelopes(toml_input).expect("parse must succeed");
        let env = &envs[0];

        let mounts_text = resolve_session_mounts(env, /* session_kind */ 0, /* vt */ 2);
        assert_eq!(mounts_text, vec![
            String::from("ro:/dev/tty2"),
            String::from("ro:/dev/null"),
        ]);

        let mounts_graphical = resolve_session_mounts(env, /* session_kind */ 1, /* vt */ 4);
        assert_eq!(mounts_graphical, vec![
            String::from("rw:/dev/pts"),
            String::from("rw:/dev/fb0"),
        ]);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p cluu-procmgr --lib envelopes::tests::vt_substitution_in_mount_paths 2>&1 | tail -15
```

Expected: `error[E0425]: cannot find function 'resolve_session_mounts'` or `parse_envelopes` rejects the new sub-tables.

- [ ] **Step 3: Commit the failing test**

```bash
git add userspace/procmgr/src/envelopes.rs
git commit -m "test: envelope vt_text/vt_graphical mounts with {vt} substitution"
```

---

## Task 2: Extend `Envelope` struct + parser

**Files:**
- Modify: `userspace/procmgr/src/envelopes.rs:20-80` (struct + parse).

- [ ] **Step 1: Add optional mount lists to `Envelope`**

In the struct definition around line 23:

```rust
#[derive(Debug, Clone)]
pub struct Envelope {
    pub name: String,
    pub mounts: Vec<MountSpec>,                 // legacy / fallback mounts
    pub env: BTreeMap<String, String>,
    pub env_template: BTreeMap<String, String>,
    /// Mount list for text VT sessions (session_kind == 0). Paths may use
    /// `{vt}` and `{user}` placeholders. If absent, falls back to `mounts`.
    pub vt_text_mounts: Vec<String>,
    /// Mount list for graphical sessions (session_kind == 1). Same
    /// placeholders. If absent, falls back to `mounts`.
    pub vt_graphical_mounts: Vec<String>,
}
```

- [ ] **Step 2: Extend `parse_envelopes`**

In `parse_envelopes` (line 51-ish), after the existing parsing of `env` and `env_template`, add:

```rust
        let vt_text_mounts: Vec<String> = envelope_table
            .get("vt_text")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("mounts"))
            .and_then(|m| m.as_array())
            .map(|arr| arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect())
            .unwrap_or_default();
        let vt_graphical_mounts: Vec<String> = envelope_table
            .get("vt_graphical")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("mounts"))
            .and_then(|m| m.as_array())
            .map(|arr| arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect())
            .unwrap_or_default();
```

And add both into the returned `Envelope { ... }`.

- [ ] **Step 3: Add `resolve_session_mounts`**

Below `resolve_env`:

```rust
/// Substitute `{vt}` and `{user}` in the appropriate mount list for the
/// session kind. `session_kind == 0` selects `vt_text_mounts`, `== 1`
/// selects `vt_graphical_mounts`. Falls back to the envelope's legacy
/// `mounts` if the chosen list is empty.
pub fn resolve_session_mounts(env: &Envelope, session_kind: u8, vt: usize) -> Vec<String> {
    let template = match session_kind {
        1 => &env.vt_graphical_mounts,
        _ => &env.vt_text_mounts,
    };
    let source: Box<dyn Iterator<Item = String>> = if template.is_empty() {
        Box::new(env.mounts.iter().map(|m| match m.mode {
            // mounts is Vec<MountSpec>; convert back to "mode:path"
            // strings for uniform downstream handling.
            MountMode::ReadOnly  => alloc::format!("ro:{}", m.path),
            MountMode::ReadWrite => alloc::format!("rw:{}", m.path),
        }))
    } else {
        Box::new(template.iter().cloned())
    };
    source
        .map(|s| s.replace("{vt}", &alloc::format!("{}", vt)))
        .collect()
}
```

(Adjust import paths for `alloc::format`, `Box`, etc. — `envelopes.rs` currently has `use alloc::...` lines near the top.)

- [ ] **Step 4: Run the test — should now pass**

```bash
cargo test -p cluu-procmgr --lib envelopes::tests::vt_substitution_in_mount_paths 2>&1 | tail -10
```

Expected: `test envelopes::tests::vt_substitution_in_mount_paths ... ok`.

- [ ] **Step 5: Run the full envelopes test module to confirm no regression**

```bash
cargo test -p cluu-procmgr --lib envelopes 2>&1 | tail -10
```

Expected: all existing tests still green.

- [ ] **Step 6: Commit**

```bash
git add userspace/procmgr/src/envelopes.rs
git commit -m "envelopes: parse vt_text/vt_graphical + resolve {vt} substitution"
```

---

## Task 3: Add `{user}` substitution to mount paths

**Files:**
- Modify: `userspace/procmgr/src/envelopes.rs` — extend `resolve_session_mounts` signature to take `user`.

- [ ] **Step 1: Failing test for `{user}` in mount paths**

Append in tests block:

```rust
    #[test]
    fn user_substitution_in_mount_paths() {
        let toml_input = r#"
[envelope.user]
[envelope.user.env]
[envelope.user.vt_text]
mounts = ["rw:/home/{user}", "rw:/tmp/{user}"]
"#;
        let envs = parse_envelopes(toml_input).expect("parse must succeed");
        let mounts = resolve_session_mounts(&envs[0], 0, 0, "alice");
        assert_eq!(mounts, vec![
            String::from("rw:/home/alice"),
            String::from("rw:/tmp/alice"),
        ]);
    }
```

(Test calls `resolve_session_mounts` with a 4-arg signature; existing signature has 3 args, so test fails to compile.)

- [ ] **Step 2: Run test, observe compile error**

```bash
cargo test -p cluu-procmgr --lib envelopes::tests::user_substitution_in_mount_paths 2>&1 | tail -10
```

Expected: `error[E0061]: this function takes 3 arguments but 4 arguments were supplied`.

- [ ] **Step 3: Update `resolve_session_mounts` signature**

```rust
pub fn resolve_session_mounts(
    env: &Envelope,
    session_kind: u8,
    vt: usize,
    user: &str,
) -> Vec<String> {
    /* ... same body, but the .map(...) chain becomes: */
    source
        .map(|s| s.replace("{vt}", &alloc::format!("{}", vt)).replace("{user}", user))
        .collect()
}
```

Update the call in the previous test (`vt_substitution_in_mount_paths`) to pass `"x"` as a placeholder user.

- [ ] **Step 4: Run both tests**

```bash
cargo test -p cluu-procmgr --lib envelopes 2>&1 | tail -10
```

Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/envelopes.rs
git commit -m "envelopes: {user} substitution in mount paths"
```

---

## Task 4: Procmgr SESSION_LOGIN uses resolved per-session mounts

**Files:**
- Modify: `userspace/procmgr/src/main.rs` — locate the two SESSION_LOGIN paths (session_kind=0 around line 2310-2503 and session_kind=1 around line 2110-2310). Each currently calls `build_view_from_envelope(&envelope)`; switch to a new path that uses `resolve_session_mounts`.

- [ ] **Step 1: Add a new builder that takes resolved mount strings**

In `userspace/procmgr/src/main.rs`, search for `fn build_view_from_envelope`. Add a sibling method:

```rust
    /// Build a ViewMountList from already-resolved mount strings.
    /// Each entry is "mode:path" — split, validate, push.
    fn build_view_from_mount_strings(strings: &[String]) -> ViewMountList {
        let mut mounts = ViewMountList::new();
        for raw in strings {
            let (mode_str, path) = match raw.split_once(':') {
                Some(p) => p,
                None => continue,
            };
            let mode = match mode_str {
                "ro" | "readonly"  => MountMode::ReadOnly,
                "rw" | "readwrite" => MountMode::ReadWrite,
                _ => continue,
            };
            mounts.push(MountSpec { mode, path: String::from(path) });
        }
        mounts
    }
```

- [ ] **Step 2: Replace the SESSION_LOGIN session_kind=0 view construction**

Locate the call site (around line 2413):

```rust
        let view_mounts = Self::build_view_from_envelope(&envelope);
```

Replace with:

```rust
        let resolved_mounts = envelopes::resolve_session_mounts(
            &envelope, /* session_kind */ 0, vt_index, &username,
        );
        let view_mounts = Self::build_view_from_mount_strings(&resolved_mounts);
```

- [ ] **Step 3: Replace the SESSION_LOGIN session_kind=1 view construction**

Around line 2218 the kind=1 branch has the equivalent line. Replace identically with `session_kind=1` and the appropriate VT (typically 4 for the compositor session; pull it from the caller — for now hard-code `let vt_index = 4;` and add a TODO to thread it through):

```rust
        let resolved_mounts = envelopes::resolve_session_mounts(
            &envelope, /* session_kind */ 1, /* vt */ 4, &username,
        );
        let view_mounts = Self::build_view_from_mount_strings(&resolved_mounts);
```

- [ ] **Step 4: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

Expected: clean build.

- [ ] **Step 5: Boot smoke**

```bash
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_cluuterm_login bash scripts/harness_run.sh
```

Existing l2_cluuterm_login marker should still pass.

- [ ] **Step 6: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: SESSION_LOGIN selects vt_text/vt_graphical mounts"
```

---

## Task 5: Update `/etc/envelopes.toml` with vt_text + vt_graphical

**Files:**
- Modify: `etc/envelopes.toml`

- [ ] **Step 1: Replace contents**

The file's current `[envelope.user]` and `[envelope.admin]` blocks keep their `env` and `env_template`. Add the two new sub-tables under each:

```toml
[envelope.user]

[envelope.user.env]
SHELL = "/bin/shell"
TERM = "cluu"
PATH = "/bin:/usr/bin"
LANG = "C"

[envelope.user.env_template]
HOME    = "/home/{user}"
USER    = "{user}"
LOGNAME = "{user}"
PWD     = "/home/{user}"

[envelope.user.vt_text]
mounts = [
    "ro:/bin", "ro:/usr", "ro:/lib", "ro:/etc",
    "ro:/dev/tty{vt}",
    "ro:/dev/null", "ro:/dev/zero", "ro:/dev/urandom",
    "rw:/home/{user}",
    "rw:/tmp",
    "ro:/proc",
]

[envelope.user.vt_graphical]
mounts = [
    "ro:/bin", "ro:/usr", "ro:/lib", "ro:/etc",
    "rw:/dev/pts",
    "rw:/dev/fb0",
    "ro:/dev/null", "ro:/dev/zero", "ro:/dev/urandom",
    "rw:/home/{user}",
    "rw:/tmp",
    "ro:/proc",
]


[envelope.admin]

[envelope.admin.env]
SHELL = "/bin/shell"
TERM = "cluu"
PATH = "/sbin:/bin:/usr/sbin:/usr/bin"
LANG = "C"

[envelope.admin.env_template]
HOME    = "/home/{user}"
USER    = "{user}"
LOGNAME = "{user}"
PWD     = "/home/{user}"

[envelope.admin.vt_text]
mounts = [
    "rw:/", "rw:/etc", "rw:/lib", "rw:/usr", "rw:/bin", "rw:/var",
    "rw:/tmp", "rw:/home",
    "rw:/dev/tty{vt}",
    "rw:/dev/null", "rw:/dev/zero", "rw:/dev/urandom",
    "rw:/dev/console",
    "rw:/proc",
]

[envelope.admin.vt_graphical]
mounts = [
    "rw:/", "rw:/etc", "rw:/lib", "rw:/usr", "rw:/bin", "rw:/var",
    "rw:/tmp", "rw:/home",
    "rw:/dev/pts", "rw:/dev/fb0",
    "rw:/dev/null", "rw:/dev/zero", "rw:/dev/urandom",
    "rw:/dev/console",
    "rw:/proc",
]
```

(Keep the file header comment block at the top.)

- [ ] **Step 2: Rebuild + smoke**

```bash
cargo xtask build 2>&1 | tail -3
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_cluuterm_login bash scripts/harness_run.sh
grep -E "shellrc|HOME" /tmp/cluu-serial-com2.log | head -10
```

Expected: no more `//.shellrc` traces — `vfs: open '/home/root/.shellrc'` instead.

- [ ] **Step 3: Commit**

```bash
git add etc/envelopes.toml
git commit -m "envelopes: add vt_text/vt_graphical mount lists for user + admin"
```

---

## Task 6: Failing harness marker — VT0 user sees only own tty

**Files:**
- Modify: `scripts/harness_case_defaults.sh` + `scripts/harness_run.sh`

- [ ] **Step 1: Add the marker entry**

In `scripts/harness_case_defaults.sh`:

```bash
            l2_envelope_dev_filter)
                TEST_COMMAND=""
                # After VT0 text login, list /dev. Expect tty0 visible,
                # tty1/tty2/tty3 NOT visible. Marker is the literal
                # `ls-dev-result: ` line that the shell builtin `ls`
                # (debug-print via shell.commands.builtins.ls) emits.
                # Sequence: f1 -> root/root -> `ls /dev` -> Enter.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 3\nsendkey ctrl-alt-f1\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey l\nsendkey s\nsendkey spc\nsendkey slash\nsendkey d\nsendkey e\nsendkey v\nsendkey ret'
                ;;
```

(`slash` key is HU-layout dependent; check `scripts/harness_run.sh:494` for `'/' -> shift-6`.)

In `scripts/harness_run.sh`:

```bash
    l2_envelope_dev_filter)
        required_markers=(
            "TSC calibrated"
            "tty:0: showing login prompt"
            "tty0"               # readdir of /dev includes tty0
        )
        forbidden_markers=(
            "tty1"               # but never tty1
            "tty2"
            "tty3"
        )
        ;;
```

`forbidden_markers` is a new mechanism — if it isn't supported by the runner yet, validate by post-grep instead. Skip the forbidden-markers section and handle it in Step 3 manually.

- [ ] **Step 2: Run it (current envelope still applies, expect mixed result)**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_envelope_dev_filter bash scripts/harness_run.sh
grep -E "tty[0-3]" /tmp/cluu-serial-com2.log | tail -20
```

- [ ] **Step 3: Confirm correctness**

After Plan 1 + Tasks 1-5 of Plan 2 land, the readdir of `/dev` from a root-on-VT0 session should list `tty0` but not `tty1..3`. If it does (and `forbidden_markers` are absent), the envelope substitution is doing its job.

If `tty1..3` still appear, the view layer isn't enforcing the narrowing — go to Task 7 (monotone audit).

- [ ] **Step 4: Commit harness**

```bash
git add scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: l2_envelope_dev_filter for per-VT /dev narrowing"
```

---

## Task 7: Monotone audit

**Files:**
- Read-only audit of `userspace/vfs/src/vfs_view.rs`, `userspace/procmgr/src/main.rs` (envelope construction + `set_view` calls), and the Cluufile `MOUNT` loader (`userspace/procmgr/src/cluufile.rs` if it exists, else grep for `MOUNT` directive parsing).

- [ ] **Step 1: Locate all `set_view` call sites**

```bash
grep -rn "set_view\b" userspace/ | grep -v "_test\|test_"
```

For each call, confirm the new view is constructed from `resolve_session_mounts` output (or is a child view derived from the parent's, never broader). Write the audit report into a temp file:

```bash
{
    echo "=== set_view audit $(date -I) ==="
    grep -n "set_view" userspace/vfs/src/vfs_view.rs userspace/procmgr/src/main.rs
    echo
    echo "=== envelope construction sites ==="
    grep -n "build_view_from\|resolve_session_mounts" userspace/procmgr/src/main.rs
} > /tmp/cluu-monotone-audit.txt
cat /tmp/cluu-monotone-audit.txt
```

- [ ] **Step 2: For each site, document parent → child relationship**

In comments (or in an audit-notes file `docs/superpowers/audits/2026-05-14-monotone-views.md`), record:

| Call site | Parent view source | Child view source | Narrows? |
|---|---|---|---|

Fill all rows. Reject any "broadens" row — must fix before continuing.

- [ ] **Step 3: Add a runtime assertion in `set_view`**

In `userspace/vfs/src/vfs_view.rs`, locate the `set_view` impl. Just before installing the new view, add a debug-mode assertion that every mount in the new view exists in the parent view (or is the parent's own initial install):

```rust
        #[cfg(debug_assertions)]
        if let Some(parent) = parent_view_lookup(parent_cid) {
            for m in new_view.iter() {
                assert!(
                    parent.contains_path(&m.path),
                    "monotone violation: child {} attempts mount {} not in parent {}",
                    child_cid, m.path, parent_cid,
                );
            }
        }
```

(`parent_view_lookup` may need to be added; if it doesn't exist as-is, skeleton it: look up the parent container's view from `vfs_view`'s own table.)

- [ ] **Step 4: Build with debug assertions; rerun all markers**

```bash
cargo xtask build 2>&1 | tail -3
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_envelope_dev_filter bash scripts/harness_run.sh
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_text_shell_input bash scripts/harness_run.sh
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_cluuterm_shell_input bash scripts/harness_run.sh
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_cluuterm_login bash scripts/harness_run.sh
```

Any panic from the new assertion is a real bug — fix at the call site that triggers it.

- [ ] **Step 5: Commit the audit + assertion**

```bash
git add userspace/vfs/src/vfs_view.rs docs/superpowers/audits/2026-05-14-monotone-views.md
git commit -m "vfs_view: debug-mode monotone assertion + audit report"
```

---

## Task 8: HOME-propagation marker

**Files:**
- Modify: harness scripts.

- [ ] **Step 1: Add marker**

In `scripts/harness_case_defaults.sh`:

```bash
            l2_envelope_home_propagated)
                TEST_COMMAND=""
                # After login, type `echo $HOME` (literally) and verify
                # `/home/root` appears in shell output via the tty service
                # forwarding writes to console.
                # printf '%s\n' "$HOME" - using echo since printf may not be
                # implemented yet.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 3\nsendkey ctrl-alt-f1\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey e\nsendkey c\nsendkey h\nsendkey o\nsendkey spc\nsendkey shift-4\nsendkey shift-h\nsendkey shift-o\nsendkey shift-m\nsendkey shift-e\nsendkey ret'
                ;;
```

(The `shift-h`-`shift-o`-`shift-m`-`shift-e` sequence types `HOME` literally; `shift-4` is `$`.)

In `scripts/harness_run.sh`:

```bash
    l2_envelope_home_propagated)
        required_markers=(
            "TSC calibrated"
            "tty:0: showing login prompt"
            "/home/root"
        )
        ;;
```

- [ ] **Step 2: Run**

```bash
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_envelope_home_propagated bash scripts/harness_run.sh
```

Marker must pass. If `/home/root` doesn't appear, either the env_template isn't being applied (back to Task 5) or the shell's variable-expansion `$HOME` isn't implemented — verify with `shell: parse` traces if available, fall back to a simpler `cat /home/root/.shellrc` test if shell variable expansion is too immature.

- [ ] **Step 3: Commit**

```bash
git add scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: l2_envelope_home_propagated marker"
```

---

## Task 9: Memory updates

**Files:**
- Add: `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_envelope_substitution.md`
- Edit: `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/MEMORY.md` (add one-line index entry).
- Edit: `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_loginCC_session_2026_05_13.md` (mark Bug B closed).

- [ ] **Step 1: Write the new project memory**

```markdown
---
name: envelope-vt-user-substitution-2026-05-14
description: "Envelopes carry vt_text + vt_graphical mount lists; procmgr substitutes {vt} and {user} at SESSION_LOGIN; monotone audit covers all set_view sites."
metadata:
  type: project
---

After plan 2026-05-14-plan2-envelope-vt-user-substitution, every user
session's VFS view is built from the envelope's `vt_text` (kind=0) or
`vt_graphical` (kind=1) mounts with `{vt}` and `{user}` substituted at
SESSION_LOGIN. Closes Bug B (HOME unset).

**Why:** Path A unification needs the right `/dev` slice per session so
shell's POSIX read(0) lands on the right `/dev/tty<N>` or `/dev/pts/<id>`.
Single-list envelopes can't express that without baking VT-specific paths
into every user record.

**How to apply:** When extending envelopes for new mounts, add to both
`vt_text` and `vt_graphical` (or just one if the mount only makes sense
in one shape). Always use `{vt}` and `{user}` for path components, never
hard-code VT indices. The runtime monotone assertion in vfs_view will
catch broadening regressions.
```

Add the one-line index entry to MEMORY.md.

In `project_loginCC_session_2026_05_13.md`, change the description and section 3 to mark Bug B CLOSED with the commit hash of Task 5.

- [ ] **Step 2: Memory edits are saved directly to ~/.claude — no git commit needed**

---

## Self-review checklist

- All `l2_envelope_*` markers green on a fresh build.
- `l2_text_shell_input`, `l2_cluuterm_shell_input`, `l2_cluuterm_login` still green (no regression from Plan 1).
- Audit doc `docs/superpowers/audits/2026-05-14-monotone-views.md` lists every set_view site and confirms narrowing.
- MEMORY.md updated; Bug B marked closed.
