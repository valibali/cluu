# Spawn env merge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `procmgr::handle_spawn_unified` merge `/etc/envelopes.toml` defaults under the caller-supplied env, so shells spawned via the unified path inherit `PATH`/`HOME`/`USER` from the user's profile.

**Architecture:** Resolve caller's session → look up user profile name from `user_records` → resolve envelope via `envelopes::lookup_envelope` → produce a `BTreeMap` via `envelopes::resolve_env(env, username)` → overlay caller's `envelope.env` on top (caller wins) → pack with existing `build_envelope_env_payload`.

**Tech Stack:** Rust (no_std + alloc), `cluu_wire::spawn::SpawnEnvelope`, `procmgr::envelopes`, `procmgr::session_table`.

Spec: `docs/superpowers/specs/2026-05-21-spawn-env-merge.md`.

---

### Task 1: Unit test for env merge precedence

**Files:**
- Modify: `userspace/procmgr/src/envelopes.rs` (add test in existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `userspace/procmgr/src/envelopes.rs`:

```rust
#[test]
fn merge_caller_env_over_resolved_envelope() {
    let envs = parse_envelopes(SAMPLE).expect("parse");
    let resolved = resolve_env(&envs[0], "balazs");
    // resolved has PATH=/bin:/usr/bin, HOME=/home/balazs

    // Caller-supplied env (simulates SpawnEnvelope.env from cluuterm).
    let caller: alloc::vec::Vec<(String, String)> = alloc::vec![
        (String::from("TERM"), String::from("xterm-256color")),
        (String::from("HOME"), String::from("/home/balazs/work")),
    ];

    // Merge: start from resolved, overlay caller.
    let mut merged = resolved.clone();
    for (k, v) in &caller {
        merged.insert(k.clone(), v.clone());
    }

    // Envelope default PATH preserved (caller did not provide).
    assert_eq!(merged.get("PATH").map(String::as_str), Some("/bin:/usr/bin"));
    // Caller HOME wins over envelope template.
    assert_eq!(merged.get("HOME").map(String::as_str), Some("/home/balazs/work"));
    // Caller-only key surfaces.
    assert_eq!(merged.get("TERM").map(String::as_str), Some("xterm-256color"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p procmgr --target x86_64-unknown-linux-gnu --lib envelopes::tests::merge_caller_env_over_resolved_envelope -- --nocapture` if a host-test target exists, otherwise: `cargo xtask test-procmgr` or `cargo test -p procmgr envelopes::tests::merge_caller_env_over_resolved_envelope`.

Expected: PASS (the test is pure data — there's no production code yet, but the merge logic is inlined in the test). This task pins the contract before the production code is added.

If the project's test runner is `cargo xtask`, use whatever target the existing tests in `envelopes.rs` already use — `parses_basic_envelope` runs there too.

- [ ] **Step 3: Commit**

```bash
git add userspace/procmgr/src/envelopes.rs
git commit -m "test(procmgr/envelopes): pin caller-wins env merge precedence"
```

---

### Task 2: Drop `#[allow(dead_code)]` annotations that block production wiring

**Files:**
- Modify: `userspace/procmgr/src/envelopes.rs:39`, `userspace/procmgr/src/envelopes.rs:52`, `userspace/procmgr/src/envelopes.rs:132`, `userspace/procmgr/src/envelopes.rs:162`

These four `#[allow(dead_code)]` markers were added because the unified spawn path didn't call them. After Task 3 they become live. Removing here keeps Task 3 minimal and surfaces any other unused arms.

- [ ] **Step 1: Remove `#[allow(dead_code)]` on `resolve_env`**

In `userspace/procmgr/src/envelopes.rs`, find:

```rust
/// Apply `{user}` substitution to env_template, merging with static env.
/// Static env wins on key conflict (matches spec §6 step 3).
#[allow(dead_code)]
pub fn resolve_env(envelope: &Envelope, user: &str) -> BTreeMap<String, String> {
```

Change to:

```rust
/// Apply `{user}` substitution to env_template, merging with static env.
/// Static env wins on key conflict (matches spec §6 step 3).
pub fn resolve_env(envelope: &Envelope, user: &str) -> BTreeMap<String, String> {
```

- [ ] **Step 2: Remove `#[allow(dead_code)]` on `lookup_envelope`**

In the same file find:

```rust
/// Look up an envelope by name in a parsed list.
#[allow(dead_code)]
pub fn lookup_envelope<'a>(envelopes: &'a [Envelope], name: &str) -> Option<&'a Envelope> {
```

Change to:

```rust
/// Look up an envelope by name in a parsed list.
pub fn lookup_envelope<'a>(envelopes: &'a [Envelope], name: &str) -> Option<&'a Envelope> {
```

- [ ] **Step 3: Leave `parse_envelopes` and `resolve_session_mounts` alone**

Those two still carry `#[allow(dead_code)]` for reasons unrelated to this plan. Touching them is out of scope.

- [ ] **Step 4: Build to confirm no other dead-code warnings appear**

Run: `cargo xtask build`
Expected: clean build, no new warnings.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/envelopes.rs
git commit -m "chore(procmgr/envelopes): drop dead_code on resolve_env/lookup_envelope (about to wire)"
```

---

### Task 3: Wire envelope env merge into `handle_spawn_unified`

**Files:**
- Modify: `userspace/procmgr/src/main.rs` (`handle_spawn_unified` body, around line 5813)

- [ ] **Step 1: Locate the existing env-pack block**

In `userspace/procmgr/src/main.rs`, `handle_spawn_unified`, find:

```rust
        // Build env payload: KEY=VALUE\0 records.
        let mut env_payload: Vec<u8> = Vec::new();
        let mut envc = 0usize;
        for (k, v) in &envelope.env {
            env_payload.extend_from_slice(k.as_bytes());
            env_payload.push(b'=');
            env_payload.extend_from_slice(v.as_bytes());
            env_payload.push(0);
            envc += 1;
        }
```

- [ ] **Step 2: Replace with session-aware merge**

Replace the block above with:

```rust
        // Resolve caller's session → username → user_records → envelope.
        // If any step fails (no enclosing session, unknown user, missing
        // envelope), fall through to the caller-only env (preserves
        // current behavior for service/boot paths).
        let merged_env: alloc::collections::BTreeMap<String, String> = {
            let resolved = self
                .resolve_caller_session(sender_tid)
                .and_then(|session| {
                    let username = session.username.clone();
                    self.user_records
                        .get(&username)
                        .map(|rec| (username, rec.profile_name.clone()))
                })
                .and_then(|(username, profile_name)| {
                    envelopes::lookup_envelope(&self.envelopes, &profile_name)
                        .map(|env_def| envelopes::resolve_env(env_def, &username))
                });

            let mut merged = resolved.unwrap_or_default();
            for (k, v) in &envelope.env {
                merged.insert(k.clone(), v.clone());
            }
            merged
        };

        // Build env payload: KEY=VALUE\0 records.
        let (env_payload, envc) = build_envelope_env_payload(&merged_env);
```

- [ ] **Step 3: Build**

Run: `cargo xtask build`
Expected: clean build.

If `build_envelope_env_payload`'s return type doesn't bind to a `Vec<u8>` `let` pattern (it returns `(Vec<u8>, usize)`), the destructure above handles it. If the compiler complains about `envc` not being mutable, that's fine — downstream code doesn't mutate it.

- [ ] **Step 4: Smoke build, check `pid_to_profile` is unchanged**

Verify no other call site that consumes `envc` as a mutable counter broke. Grep:

Run: `rg -n 'envc\s*=' userspace/procmgr/src/main.rs`
Expected: only the new binding from `build_envelope_env_payload` and pre-existing uses in `spawn_service_with_env` callers.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "feat(procmgr): merge envelope env defaults in handle_spawn_unified

Caller-supplied SpawnEnvelope.env now overlays the resolved
/etc/envelopes.toml profile env. Login → cluuterm → shell chain
gets PATH/HOME/USER for free, single source of truth.

Service / boot paths (no enclosing session) keep caller-only env."
```

---

### Task 4: Boot-time integration smoke

**Files:** none (runtime check)

- [ ] **Step 1: Build a fresh image**

Run: `rm -rf target/newlib-build target/sysroot/x86_64-cluu-elf && make clean && cargo xtask build-newlib && cargo xtask build-syscalls && cargo xtask build-crt0 && cargo xtask build`

Or, if no newlib changes are pending: `cargo xtask build`
Expected: clean.

- [ ] **Step 2: Run the harness, drive login, watch shell stdin**

Run the standard headless harness (see `feedback_harness_autostart`):

```bash
HARNESS_FORCE_BUILD=1 bash scripts/harness_run.sh
```

Drive through login interactively (auto-driver script if one exists for login). When shell prompt appears, type `env\n` (or whatever lists env in this shell).

Expected (in serial log):
- `PATH=/bin:/usr/bin`
- `HOME=/home/<user>`
- `USER=<user>`
- `TERM=xterm-256color` (cluuterm's override beats envelope default `cluu`)

- [ ] **Step 3: Confirm `ls` parses as a known PATH lookup**

Type `ls\n`. The previous failure mode was `shell: unsupported command`. After the fix, the shell should walk `$PATH` and (since `/bin/ls` is not necessarily wired) either spawn or fail with a different error (image not found, exec failed). Either is acceptable — the point is that PATH resolution actually runs.

- [ ] **Step 4: Confirm service spawn unchanged**

Watch boot log for the usual `procmgr: spawn_service_with_env ...` lines. None should now fail or change env composition — they don't traverse `handle_spawn_unified`.

If they do (CLUU's spawn unification is partly bypassed today per `project_spawn_hooks_unwired`), verify `[envelope.service]` defaults still hit services that route through unified. Specifically, no service should suddenly get `[envelope.user]` env — `resolve_caller_session` returns `None` for primordials.

- [ ] **Step 5: Commit harness logs if useful**

If the harness emits a marker file or smoke artifact this plan should pin, capture it; otherwise no commit on this task.

---

### Task 5: Memory update

**Files:**
- Modify: `~/.claude/projects/-home-vlb2bp-git-cluu/memory/MEMORY.md` (index pointer)
- Create: `~/.claude/projects/-home-vlb2bp-git-cluu/memory/project_spawn_env_merge.md`

- [ ] **Step 1: Write memory entry**

Create `project_spawn_env_merge.md` with type `project`:

```markdown
---
name: spawn-env-merge
description: handle_spawn_unified now merges /etc/envelopes.toml defaults under caller env (caller wins). Login→cluuterm→shell sees PATH/HOME/USER.
metadata:
  type: project
---

`procmgr::handle_spawn_unified` resolves caller session → username →
`user_records[username].profile_name` → envelope, then `resolve_env`,
overlays `SpawnEnvelope.env` on top. No-session path (boot/services)
unchanged.

**Why:** Shell logged in via cluuterm rejected `ls` because PATH was
empty. cluuterm only forwarded `TERM`; login only forwarded `HOME/USER`.

**How to apply:** Don't reintroduce env hardcoding in cluuterm or new
spawners — let procmgr's merge cover defaults. Override per-key in
`SpawnEnvelope.env` when needed.

Spec: `docs/superpowers/specs/2026-05-21-spawn-env-merge.md`.

Related: [[project_envelope_substitution_2026_05_14]],
[[project_spawn_hooks_unwired]].
```

- [ ] **Step 2: Add MEMORY.md pointer**

Insert under the "## Index" section:

```markdown
- [Spawn env merge in handle_spawn_unified (2026-05-21)](project_spawn_env_merge.md) — procmgr merges envelope defaults, caller wins. Fixes PATH/HOME/USER leak from login→cluuterm→shell.
```

- [ ] **Step 3: No git commit for memory files**

Memory lives outside the repo. Skip git operations for this step.

---

## Self-review

- **Spec coverage:** Tasks 1 (precedence), 3 (wiring), 4 (acceptance smoke) cover the spec's Acceptance section. Task 4 covers "no regression in service spawn".
- **Placeholders:** none.
- **Type consistency:** `merged_env: BTreeMap<String, String>` matches `build_envelope_env_payload`'s `&BTreeMap<String, String>` signature; `resolve_env` returns the same type; `envelope.env: Vec<(String, String)>` insert path is `merged.insert(k.clone(), v.clone())`.
