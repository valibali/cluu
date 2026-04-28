# User Envelope Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the session-login user envelope so every spawned binary inherits sensible mount + env defaults, plus shell PATH/export/shellrc machinery — closing the MicroPython `open('/etc/motd')` bug systemically.

**Architecture:** Per-profile-class envelopes from `/etc/envelopes.toml` resolve at session-login into mount + env spawn blocks for the shell. Cluufile MOUNT directives narrow within the envelope strictly (mismatch fails spawn). Shell gains POSIX `export`, PATH lookup, `~/.shellrc` sourcing, one-way env mirror to newlib.

**Tech Stack:** Rust no_std for procmgr/libcluu/shell/vfs, TOML parsing via existing `toml` workspace dep, x86_64-cluu-elf, QEMU-based test harness.

**Spec:** `docs/superpowers/specs/2026-04-28-user-envelope-design.md`

---

## Phase 1 — `/etc/envelopes.toml` and procmgr parser

### Task 1: Envelope/MountSpec types in procmgr

**Files:**
- Create: `userspace/procmgr/src/envelopes.rs`

- [ ] **Step 1: Create envelopes.rs with type definitions**

```rust
//! Per-profile-class user envelope definitions.
//!
//! Loaded from /etc/envelopes.toml at procmgr boot. Each user record in
//! /etc/users.toml has a profile field that selects an envelope. The
//! envelope provides the mount view + env defaults at session-login.
//!
//! See docs/superpowers/specs/2026-04-28-user-envelope-design.md.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MountMode {
    Ro,
    Rw,
}

#[derive(Clone, Debug)]
pub struct MountSpec {
    pub path: String,
    pub mode: MountMode,
}

#[derive(Clone, Debug)]
pub struct Envelope {
    pub name: String,
    pub mounts: Vec<MountSpec>,
    pub env: BTreeMap<String, String>,
    pub env_template: BTreeMap<String, String>,
}

/// Apply {user} substitution to env_template, merging with static env.
/// Static env wins on key conflict (matches spec §6 step 3).
pub fn resolve_env(
    envelope: &Envelope,
    user: &str,
) -> BTreeMap<String, String> {
    let mut out = envelope.env.clone();
    for (k, template) in &envelope.env_template {
        let resolved = template.replace("{user}", user);
        out.entry(k.clone()).or_insert(resolved);
    }
    out
}
```

- [ ] **Step 2: Wire module into procmgr**

In `userspace/procmgr/src/main.rs`, add near the top with other `mod` declarations:
```rust
mod envelopes;
```

- [ ] **Step 3: Build**

Run: `cargo xtask build`
Expected: success. Functions are unused but `pub`-declared, so no warnings.

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/envelopes.rs userspace/procmgr/src/main.rs
git commit -m "procmgr: scaffold Envelope/MountSpec types in envelopes.rs"
```

---

### Task 2: TOML parser for envelopes.toml

**Files:**
- Modify: `userspace/procmgr/src/envelopes.rs`

- [ ] **Step 1: Add the parse function**

Append to `envelopes.rs`:

```rust
/// Parse the contents of /etc/envelopes.toml into a list of Envelopes.
/// Returns `Err(reason)` on malformed input — caller (procmgr boot)
/// should panic on Err since boot can't proceed without valid envelopes.
pub fn parse_envelopes(toml_str: &str) -> Result<Vec<Envelope>, alloc::string::String> {
    use alloc::format;
    use alloc::string::ToString;

    let parsed: toml::Value = toml::from_str(toml_str)
        .map_err(|e| format!("envelopes.toml: {}", e))?;

    let table = parsed.as_table()
        .ok_or_else(|| "envelopes.toml: top level must be a table".to_string())?;

    let envelopes_arr = table.get("envelope")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "envelopes.toml: missing [[envelope]] array".to_string())?;

    let mut out = Vec::with_capacity(envelopes_arr.len());
    for (idx, env_val) in envelopes_arr.iter().enumerate() {
        let env_table = env_val.as_table()
            .ok_or_else(|| format!("[[envelope]] {} not a table", idx))?;

        let name = env_table.get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("[[envelope]] {} missing name", idx))?
            .to_string();

        let mut mounts = Vec::new();
        if let Some(mounts_arr) = env_table.get("mounts").and_then(|v| v.as_array()) {
            for (m_idx, m_val) in mounts_arr.iter().enumerate() {
                let m_table = m_val.as_table()
                    .ok_or_else(|| format!("envelope '{}' mount {} not a table", name, m_idx))?;
                let path = m_table.get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("envelope '{}' mount {} missing path", name, m_idx))?
                    .to_string();
                let mode_str = m_table.get("mode")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("envelope '{}' mount {} missing mode", name, m_idx))?;
                let mode = match mode_str {
                    "ro" | "readonly" => MountMode::Ro,
                    "rw" | "readwrite" => MountMode::Rw,
                    other => return Err(format!("envelope '{}' mount {} unknown mode '{}'", name, m_idx, other)),
                };
                mounts.push(MountSpec { path, mode });
            }
        }

        let mut env = BTreeMap::new();
        if let Some(env_tbl) = env_table.get("env").and_then(|v| v.as_table()) {
            for (k, v) in env_tbl {
                if let Some(s) = v.as_str() {
                    env.insert(k.to_string(), s.to_string());
                }
            }
        }

        let mut env_template = BTreeMap::new();
        if let Some(env_tbl) = env_table.get("env_template").and_then(|v| v.as_table()) {
            for (k, v) in env_tbl {
                if let Some(s) = v.as_str() {
                    env_template.insert(k.to_string(), s.to_string());
                }
            }
        }

        out.push(Envelope { name, mounts, env, env_template });
    }
    Ok(out)
}

/// Look up an envelope by name in a parsed list.
pub fn lookup_envelope<'a>(envelopes: &'a [Envelope], name: &str) -> Option<&'a Envelope> {
    envelopes.iter().find(|e| e.name == name)
}
```

- [ ] **Step 2: Build**

Run: `cargo xtask build`
Expected: success. The `toml` crate is already a workspace dep (used by users.toml parser).

- [ ] **Step 3: Add a unit test**

Append at the bottom of `envelopes.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[envelope]]
name = "user"
mounts = [
    { path = "/etc", mode = "ro" },
    { path = "/tmp", mode = "rw" },
]

[envelope.env]
PATH = "/bin:/usr/bin"

[envelope.env_template]
HOME = "/home/{user}"
"#;

    #[test]
    fn parses_basic_envelope() {
        let envs = parse_envelopes(SAMPLE).expect("parse");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "user");
        assert_eq!(envs[0].mounts.len(), 2);
        assert_eq!(envs[0].mounts[0].path, "/etc");
        assert_eq!(envs[0].mounts[0].mode, MountMode::Ro);
        assert_eq!(envs[0].env.get("PATH").unwrap(), "/bin:/usr/bin");
        assert_eq!(envs[0].env_template.get("HOME").unwrap(), "/home/{user}");
    }

    #[test]
    fn substitutes_user_template() {
        let envs = parse_envelopes(SAMPLE).expect("parse");
        let resolved = resolve_env(&envs[0], "balazs");
        assert_eq!(resolved.get("HOME").unwrap(), "/home/balazs");
        assert_eq!(resolved.get("PATH").unwrap(), "/bin:/usr/bin");
    }

    #[test]
    fn rejects_bad_mode() {
        let bad = r#"
[[envelope]]
name = "x"
mounts = [{ path = "/", mode = "weird" }]
"#;
        assert!(parse_envelopes(bad).is_err());
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p procmgr envelopes::tests`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/envelopes.rs
git commit -m "procmgr/envelopes: TOML parser + resolve_env helper"
```

---

### Task 3: Ship `etc/envelopes.toml` and stage to userdisk

**Files:**
- Create: `etc/envelopes.toml`
- Modify: `xtask/src/main.rs` (find the existing /etc-staging block; add envelopes.toml beside motd, users.toml etc)

- [ ] **Step 1: Create the file**

Create `etc/envelopes.toml` with the full contents from spec §5 (admin/user/service envelopes).

- [ ] **Step 2: Find the existing /etc copy block in xtask**

Run: `grep -n "users.toml\|motd\|/etc/" xtask/src/main.rs | head -20`
Expected: shows the userdisk-staging code. Note the line numbers.

- [ ] **Step 3: Add envelopes.toml staging**

Locate where `users.toml` is copied into the userdisk image. Add a parallel line for `envelopes.toml`. Pattern:
```rust
copy_to_userdisk("etc/envelopes.toml", "/etc/envelopes.toml")?;
```
(Adapt to whatever helper xtask actually uses.)

- [ ] **Step 4: Build**

Run: `cargo xtask build`
Expected: success. Build log shows `[userdisk] Added /etc/envelopes.toml`.

- [ ] **Step 5: Commit**

```bash
git add etc/envelopes.toml xtask/src/main.rs
git commit -m "etc/envelopes.toml: ship admin/user/service envelopes; xtask staging"
```

---

### Task 4: Procmgr loads envelopes.toml at boot

**Files:**
- Modify: `userspace/procmgr/src/main.rs`

- [ ] **Step 1: Add envelopes field to ProcessManager**

Find the `struct ProcessManager` definition (around line 204). Add field:
```rust
envelopes: alloc::vec::Vec<crate::envelopes::Envelope>,
```

In the constructor (`fn new()` around line 272), initialize:
```rust
envelopes: alloc::vec::Vec::new(),
```

- [ ] **Step 2: Add load_envelopes method**

Add a new method on `ProcessManager`:
```rust
fn load_envelopes(&mut self) {
    let data = match self.read_file_from_vfs("/etc/envelopes.toml") {
        Some(data) => data,
        None => {
            let _ = debug_print("procmgr: /etc/envelopes.toml missing — boot will hang");
            panic!("envelopes.toml missing");
        }
    };
    let toml_str = match core::str::from_utf8(&data) {
        Ok(s) => s,
        Err(_) => {
            let _ = debug_print("procmgr: /etc/envelopes.toml not UTF-8");
            panic!("envelopes.toml not UTF-8");
        }
    };
    match crate::envelopes::parse_envelopes(toml_str) {
        Ok(envs) => {
            let _ = debug_print(&alloc::format!(
                "procmgr: loaded {} envelopes from /etc/envelopes.toml",
                envs.len()
            ));
            self.envelopes = envs;
        }
        Err(e) => {
            let _ = debug_print(&alloc::format!("procmgr: envelopes.toml parse error: {}", e));
            panic!("envelopes.toml parse error");
        }
    }
}
```

- [ ] **Step 3: Call load_envelopes during init**

Find where `users.toml` is loaded (`grep -n load_users\|users\.toml userspace/procmgr/src/main.rs`). Add a call to `self.load_envelopes()` immediately after `load_users` runs.

- [ ] **Step 4: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 5: Run m1_recv smoke**

Run: `scripts/harness_run.sh m1_recv`
Expected: PASS. Boot log shows `procmgr: loaded 3 envelopes from /etc/envelopes.toml`.

- [ ] **Step 6: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: load /etc/envelopes.toml at boot; panic on malformed"
```

---

## Phase 2 — Mount mode (rw/ro) plumbing

### Task 5: Extend `MountSpec` and `MountPolicy` types

**Files:**
- Modify: `userspace/procmgr/src/mount_policy.rs`

- [ ] **Step 1: Add MountMode field to existing types**

Open `mount_policy.rs`. Find the existing `MountPolicy` enum and `MountPolicyEntry` struct. Add a new `mode: MountMode` field to `MountPolicyEntry`:

```rust
use crate::envelopes::MountMode;

#[derive(Clone, Debug)]
pub struct MountPolicyEntry {
    pub path: String,
    pub policy: MountPolicy,
    pub mode: MountMode,  // NEW
}
```

Update all construction sites in this file to set `mode`. For `default_mount_policies` (line ~109), use `MountMode::Rw` for /tmp and /log (matches existing implicit behavior).

- [ ] **Step 2: Build**

Run: `cargo xtask build`
Expected: build fails on construction sites. Fix each site by adding `mode: MountMode::Rw` (or `Ro` for paths that should be read-only — typically only system paths). Keep this conservative; default to Rw to match existing behavior unless we specifically want to lock down.

- [ ] **Step 3: Update tests in mount_policy.rs**

Existing `#[cfg(test)]` tests construct `MountPolicyEntry`. Update each to include `mode: MountMode::Rw` (or `Ro` where the test scenario requires).

- [ ] **Step 4: Run tests**

Run: `cargo test -p procmgr mount_policy`
Expected: all green after construction-site fixes.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/mount_policy.rs
git commit -m "procmgr/mount_policy: MountPolicyEntry gains mode (Ro/Rw) field"
```

---

### Task 6: VFS_SET_VIEW wire format extension (writable bit per mount)

**Files:**
- Modify: `userspace/libcluu/src/ipc.rs` (the SET_VIEW payload builder)
- Modify: `userspace/vfs/src/main.rs` (the SET_VIEW handler)

- [ ] **Step 1: Find existing wire format**

Run: `grep -n "VFS_SET_VIEW\|set_view\|build_set_view" userspace/libcluu/src/ipc.rs userspace/vfs/src/main.rs | head -10`

Today's payload packs each mount as fixed-size record. Identify the per-mount struct.

- [ ] **Step 2: Add writable bit**

In the per-mount record, add a 1-byte `writable: u8` field (1 = rw, 0 = ro). For backwards-compat: place it AFTER the existing fields so old readers (none expected) ignore.

Update the builder to set `writable` per `MountSpec.mode`.

- [ ] **Step 3: VFS-side: parse and store writable bit**

In `vfs/src/main.rs`, find the SET_VIEW handler. Each parsed mount entry stores the `writable: bool` in the per-process view table. Existing `VfsMount` struct gains the field.

- [ ] **Step 4: VFS-side: enforce on open**

In the `vfs::open()` handler, when client opens a file with `O_WRONLY` or `O_RDWR`, look up the path's containing mount in the client's view; if `writable == false`, return `Error::PermissionDenied` (errno = EACCES at libcluu translation).

- [ ] **Step 5: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 6: Smoke**

Run: `scripts/harness_run.sh m1_recv`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add userspace/libcluu/src/ipc.rs userspace/vfs/src/main.rs
git commit -m "vfs: SET_VIEW carries writable bit per mount; open enforces ro"
```

---

### Task 7: Test that ro/rw enforcement works

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Add l2_envelope_mounts harness case**

In `scripts/harness_cases.conf`:
```
l2_envelope_mounts|full|MARKER_MODE=l2_envelope_mounts TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

In `scripts/harness_case_defaults.sh`:
```bash
l2_envelope_mounts)
    TEST_COMMAND=""
    SHELL_AUTOSTART_CMD_DEFAULT="spawn touch /etc/probefile"
    ;;
```

In `scripts/harness_run.sh`:
```bash
l2_envelope_mounts)
    required_markers=(
        "TSC calibrated"
        "[USER] shell: ready"
        "touch: /etc/probefile: PermissionDenied"
    )
    ;;
```

(touch will fail on writing /etc — proves /etc is RO under user envelope.)

- [ ] **Step 2: Run case**

Run: `scripts/harness_run.sh l2_envelope_mounts`
Expected: PASS once Phase 3 lands (envelope is fed into the shell's view). For now this case will likely fail because the envelope plumbing isn't connected yet. **That's the TDD red.** Don't fix yet; the next phase will.

- [ ] **Step 3: Commit (red test)**

```bash
git add scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: l2_envelope_mounts (RED — touches /etc, expects EACCES)"
```

---

## Phase 3 — Session-login envelope resolution

### Task 8: Resolve envelope at session-login

**Files:**
- Modify: `userspace/procmgr/src/main.rs` (the `handle_session_login` function)

- [ ] **Step 1: Locate handle_session_login**

Run: `grep -n "fn handle_session_login\|session_login" userspace/procmgr/src/main.rs | head -5`

- [ ] **Step 2: After user is authenticated, resolve envelope**

Inside `handle_session_login`, after the `users.toml` lookup succeeds and the user is authenticated, add envelope resolution:

```rust
// Resolve envelope by user's profile name.
let envelope = match crate::envelopes::lookup_envelope(&self.envelopes, &user.profile_name) {
    Some(e) => e.clone(),
    None => {
        let _ = debug_print(&format!(
            "procmgr: session-login fail: no envelope for profile '{}'",
            user.profile_name
        ));
        // Reject login — same path as bad-password rejection.
        return self.reject_login(reply_token, sender_tid);
    }
};

// Substitute {user} in env_template, then merge with static env.
let resolved_env = crate::envelopes::resolve_env(&envelope, &user.username);
```

(Adapt method names like `reject_login` to whatever exists.)

- [ ] **Step 3: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr/session-login: resolve envelope; reject login if profile unknown"
```

---

### Task 9: Build env block from resolved env

**Files:**
- Modify: `userspace/procmgr/src/main.rs` (`spawn_service_with_env` callsite in handle_session_login)

- [ ] **Step 1: Convert resolved_env BTreeMap into env_data + envc payload**

Today, env data is built via a function like `build_default_env_payload()` (returns `Vec<u8>` and count). Add (or modify) an analog that takes a `BTreeMap<String, String>`:

```rust
fn build_env_payload(env: &alloc::collections::BTreeMap<alloc::string::String, alloc::string::String>) -> (alloc::vec::Vec<u8>, usize) {
    let mut payload = alloc::vec::Vec::new();
    let mut count = 0;
    for (k, v) in env {
        // Format: KEY=VALUE\0
        payload.extend_from_slice(k.as_bytes());
        payload.push(b'=');
        payload.extend_from_slice(v.as_bytes());
        payload.push(0);
        count += 1;
    }
    (payload, count)
}
```

- [ ] **Step 2: Pass env to spawn**

Replace the existing call to `spawn_service_with_env(...)` in handle_session_login with one that passes the resolved env:

```rust
let (env_data, envc) = build_env_payload(&resolved_env);
match self.spawn_service_with_env(
    SERVICE_PATH,                 // "/var/images/vt/bin/shell" or similar
    DEFAULT_PRIORITY,
    &shell_argv_payload,
    shell_argc,
    &env_data,
    envc,
    /* owner_tid */ sender_tid,
    spawn_seq,
    spawn_start,
    /* fdac */ &[],
    /* profile */ user.profile,
    /* extra tokens */ 0, 0,
    /* param overrides */ &[],
    /* caller_view */ None,
    /* cwd */ resolved_env.get("PWD").map(|s| s.as_bytes()).unwrap_or(b""),
    /* thread_flags */ 0,
) { ... }
```

(Adapt arg names and order to what `spawn_service_with_env` actually wants.)

- [ ] **Step 3: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr/session-login: env_data from resolved envelope (replaces hardcoded defaults)"
```

---

### Task 10: Build mount list from envelope; pass via VFS_SET_VIEW

**Files:**
- Modify: `userspace/procmgr/src/main.rs`

- [ ] **Step 1: Convert envelope.mounts to ViewMountList**

Find the existing `ViewMountList` building code (typically in handle_session_login or spawn_service_with_env). Replace the hardcoded mount list with one derived from `envelope.mounts`:

```rust
let mut view_mounts = ViewMountList::new();
for m in &envelope.mounts {
    view_mounts.push(ViewMount {
        path: m.path.clone(),
        memfs_cid: 0,            // memfs-backed only for `private` Cluufile mounts
        writable: matches!(m.mode, crate::envelopes::MountMode::Rw),
    });
}
```

- [ ] **Step 2: Pass view_mounts down**

Plumb `view_mounts` into the SET_VIEW call that fires once the shell process is created.

- [ ] **Step 3: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 4: Run l2_envelope_mounts harness case**

Run: `scripts/harness_run.sh l2_envelope_mounts`
Expected: PASS now (touch fails on /etc with PermissionDenied).

- [ ] **Step 5: Run m1_recv regression**

Run: `scripts/harness_run.sh m1_recv`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr/session-login: ViewMountList from envelope; l2_envelope_mounts green"
```

---

### Task 11: Test that env vars are set correctly

**Files:**
- Modify: `userspace/c-programs/envprobe.c` (extend to take argv keys)
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Extend envprobe to accept env var names as argv**

If envprobe today prints a fixed list, extend it so `envprobe FOO BAR` prints each key=value to debug_print. Specific change:

```c
int main(int argc, char **argv) {
    debug_print("envprobe: start");
    for (int i = 1; i < argc; i++) {
        const char *val = getenv(argv[i]);
        char buf[256];
        snprintf(buf, sizeof buf, "envprobe: %s=%s", argv[i], val ? val : "(null)");
        debug_print(buf);
    }
    debug_print("envprobe: done");
    return 0;
}
```

- [ ] **Step 2: Add l2_envelope_user case**

In `scripts/harness_cases.conf`:
```
l2_envelope_user|full|MARKER_MODE=l2_envelope_user TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

In `scripts/harness_case_defaults.sh`:
```bash
l2_envelope_user)
    TEST_COMMAND=""
    SHELL_AUTOSTART_CMD_DEFAULT="spawn envprobe HOME USER PATH SHELL"
    ;;
```

In `scripts/harness_run.sh`:
```bash
l2_envelope_user)
    required_markers=(
        "TSC calibrated"
        "[USER] shell: ready"
        "envprobe: HOME=/home/balazs"
        "envprobe: USER=balazs"
        "envprobe: PATH=/bin:/usr/bin"
        "envprobe: SHELL=/bin/shell"
    )
    ;;
```

(Adjust username if test login uses something other than `balazs`.)

- [ ] **Step 3: Run case**

Run: `scripts/harness_run.sh l2_envelope_user`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add userspace/c-programs/envprobe.c scripts/harness_*
git commit -m "harness: l2_envelope_user — verify HOME/USER/PATH/SHELL set from envelope"
```

---

## Phase 4 — Cluufile composition (strict)

### Task 12: Cluufile parser accepts `ro/rw/readonly/readwrite/private`

**Files:**
- Modify: `userspace/procmgr/src/mount_policy.rs` (parse_mount_policies_raw — the Cluufile parser side)

- [ ] **Step 1: Find existing keyword parsing**

The current `MOUNT <path> <keyword>` accepts `inherit | private`. Locate it:

Run: `grep -n "fn parse_mount_policies_raw\|inherit\|private" userspace/procmgr/src/mount_policy.rs | head -10`

- [ ] **Step 2: Extend keyword list**

Add `ro / readonly` → `MountPolicy::Inherit` + `MountMode::Ro`,
and `rw / readwrite` → `MountPolicy::Inherit` + `MountMode::Rw`.

`inherit` (no mode) → keep as `MountPolicy::Inherit` + mode inherited from parent.
`private` → `MountPolicy::Private` + `MountMode::Rw` (default).

Existing tests for the parser need to be updated to assert `mode` in addition to `policy`.

- [ ] **Step 3: Run parser tests**

Run: `cargo test -p procmgr mount_policy::tests`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/mount_policy.rs
git commit -m "procmgr/mount_policy: Cluufile parser accepts ro/rw/readonly/readwrite mode keywords"
```

---

### Task 13: Strict Cluufile validation (mismatch fails spawn)

**Files:**
- Modify: `userspace/procmgr/src/mount_policy.rs` (or wherever Cluufile-vs-parent-view resolution happens)
- Modify: `userspace/procmgr/src/main.rs` (caller of resolution, propagate error)

- [ ] **Step 1: Add strict-resolve function**

Create a new function:

```rust
/// Validate that every Cluufile MOUNT directive can be satisfied by the
/// parent's view. Returns Err with reason if any directive demands more
/// than the parent provides.
///
/// Spec §7 strict mode (Q4: Y).
pub fn validate_cluufile_against_parent(
    cluufile_entries: &[MountPolicyEntry],
    parent_view: &[ViewMount],
) -> Result<(), alloc::string::String> {
    use alloc::format;
    for cl_entry in cluufile_entries {
        // Look up cl_entry.path in parent_view (longest-prefix-match).
        let pv = parent_view.iter()
            .filter(|v| cl_entry.path.starts_with(&v.path))
            .max_by_key(|v| v.path.len())
            .ok_or_else(|| format!(
                "cluufile mismatch: requires {}, parent does not provide",
                cl_entry.path
            ))?;

        // If Cluufile asks Rw but parent has Ro, fail.
        if matches!(cl_entry.mode, MountMode::Rw) && !pv.writable {
            return Err(format!(
                "cluufile mismatch: requires {} rw, parent has ro",
                cl_entry.path
            ));
        }
    }
    Ok(())
}
```

- [ ] **Step 2: Hook into handle_container_run**

In procmgr's `handle_container_run`, after the Cluufile is parsed and the parent view is loaded, call `validate_cluufile_against_parent`. On error: log + reply with status `Error::PermissionDenied` (which maps to exit cookie 126 in the shell).

- [ ] **Step 3: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 4: Run l2_cluufile_match (will need a mismatch probe)**

Defer to next task.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/mount_policy.rs userspace/procmgr/src/main.rs
git commit -m "procmgr: validate Cluufile against parent view; mismatch returns EACCES"
```

---

### Task 14: l2_cluufile_match + l2_cluufile_mismatch harness cases

**Files:**
- Create: `userspace/c-programs/clusafilemismatch.c` (a probe whose Cluufile demands `MOUNT /etc readwrite`)
- Create: `containers/cfmismatch/Cluufile`
- Modify: workspace `Cargo.toml`, `xtask/src/main.rs`, `scripts/harness_*`

- [ ] **Step 1: Create the mismatch probe**

```c
// userspace/c-programs/cfmismatch.c
#include <stdio.h>
extern void debug_print(const char *msg);
int main(void) {
    debug_print("cfmismatch: should never run under user envelope");
    return 0;
}
```

(Cluufile demands /etc rw; user envelope has ro; spawn must fail before main runs.)

- [ ] **Step 2: Create Cluufile**

```
# containers/cfmismatch/Cluufile
PROFILE ipc vfs
MOUNT /etc readwrite
ENTRYPOINT /bin/cfmismatch
```

- [ ] **Step 3: Wire into xtask + workspace.Cargo.toml**

(Same plumbing as cat/grep/etc.)

- [ ] **Step 4: Add l2_cluufile_match harness case**

```
l2_cluufile_match|full|MARKER_MODE=l2_cluufile_match TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

```bash
l2_cluufile_match)
    TEST_COMMAND=""
    SHELL_AUTOSTART_CMD_DEFAULT="cat /etc/motd"
    ;;
```

```bash
l2_cluufile_match)
    required_markers=(
        "TSC calibrated"
        "[USER] shell: ready"
        "Welcome to CLUU"
    )
    ;;
```

- [ ] **Step 5: Add l2_cluufile_mismatch harness case**

```
l2_cluufile_mismatch|full|MARKER_MODE=l2_cluufile_mismatch TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

```bash
l2_cluufile_mismatch)
    TEST_COMMAND=""
    SHELL_AUTOSTART_CMD_DEFAULT="spawn cfmismatch"
    ;;
```

```bash
l2_cluufile_mismatch)
    required_markers=(
        "TSC calibrated"
        "[USER] shell: ready"
        "procmgr: cluufile mismatch"
    )
    ;;
```

- [ ] **Step 6: Run both cases**

Run: `scripts/harness_run.sh l2_cluufile_match`
Expected: PASS.

Run: `scripts/harness_run.sh l2_cluufile_mismatch`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add userspace/c-programs/cfmismatch.c containers/cfmismatch/ Cargo.toml xtask/src/main.rs scripts/harness_*
git commit -m "harness: l2_cluufile_match + l2_cluufile_mismatch (strict Cluufile validation)"
```

---

## Phase 5 — Shell PATH lookup + export semantics

### Task 15: CommandContext.exported set + export builtin

**Files:**
- Modify: `userspace/shell/src/commands.rs`

- [ ] **Step 1: Add exported field to CommandContext**

Find `struct CommandContext` (around line 45). Add field:
```rust
exported: alloc::collections::BTreeSet<alloc::string::String>,
```

In `CommandContext::new()`, initialize `exported: BTreeSet::new()`.

- [ ] **Step 2: Add ExportBuiltin**

Add a new builtin struct following the existing pattern (similar to SetBuiltin):

```rust
struct ExportBuiltin;

impl BuiltinCommand for ExportBuiltin {
    fn name(&self) -> &'static str { "export" }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        if args.is_empty() {
            // print all exported vars
            for name in context.exported.iter() {
                if let Some(v) = context.get(name) {
                    let line = format!("export {}={}\n", name, v);
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
                }
            }
            return Ok(());
        }
        for arg in args {
            if let Some(eq) = arg.find('=') {
                let (k, v) = arg.split_at(eq);
                let value = &v[1..]; // skip '='
                context.set(k, value.to_string());
                context.exported.insert(k.to_string());
            } else {
                // Just mark for export (if it exists in vars)
                context.exported.insert(arg.clone());
            }
        }
        Ok(())
    }
}
```

Register it in `DefaultBuiltins::register` near the other env-related builtins.

- [ ] **Step 3: UnsetBuiltin removes from exported too**

Find existing `UnsetBuiltin`. After removing from `vars`, also remove from `exported`.

- [ ] **Step 4: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add userspace/shell/src/commands.rs
git commit -m "shell: ExportBuiltin + CommandContext.exported set"
```

---

### Task 16: Spawn payload uses (vars ∩ exported) ∪ envelope env

**Files:**
- Modify: `userspace/shell/src/commands.rs` (spawn_process_with_argv)

- [ ] **Step 1: Build env block before spawn**

In `spawn_process_with_argv`, before calling `call_with_payload`, build the env block. The shell's own env (received from procmgr at startup via newlib's _environ) is the envelope-resolved env. Layer:

```rust
let mut env_pairs: Vec<(String, String)> = Vec::new();

// Start with shell's _environ (envelope-resolved at session-login)
{
    extern "C" {
        static environ: *const *const c_char;
    }
    let mut p = unsafe { environ };
    while !unsafe { *p }.is_null() {
        let s = unsafe { core::ffi::CStr::from_ptr(*p) }.to_string_lossy().into_owned();
        if let Some(eq) = s.find('=') {
            env_pairs.push((s[..eq].to_string(), s[eq+1..].to_string()));
        }
        p = unsafe { p.add(1) };
    }
}

// Override with shell's exported vars
for name in context.exported.iter() {
    if let Some(v) = context.get(name) {
        // Replace if exists, otherwise append
        if let Some(idx) = env_pairs.iter().position(|(k, _)| k == name) {
            env_pairs[idx].1 = v.to_string();
        } else {
            env_pairs.push((name.clone(), v.to_string()));
        }
    }
}
```

Then pack `env_pairs` into the existing env trailer of the CONTAINER_RUN payload.

- [ ] **Step 2: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 3: Add l2_export harness case**

In `scripts/harness_cases.conf`:
```
l2_export|full|MARKER_MODE=l2_export TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

In `scripts/harness_case_defaults.sh`:
```bash
l2_export)
    TEST_COMMAND=""
    SHELL_AUTOSTART_CMD_DEFAULT="set X=local; export Y=exported; spawn envprobe X Y"
    ;;
```

In `scripts/harness_run.sh`:
```bash
l2_export)
    required_markers=(
        "TSC calibrated"
        "[USER] shell: ready"
        "envprobe: X=(null)"
        "envprobe: Y=exported"
    )
    ;;
```

- [ ] **Step 4: Run case**

Run: `scripts/harness_run.sh l2_export`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add userspace/shell/src/commands.rs scripts/harness_*
git commit -m "shell: spawn passes (envelope env) overlaid with (vars ∩ exported); l2_export green"
```

---

### Task 17: PATH-based bare-command resolution

**Files:**
- Create: `userspace/shell/src/path_lookup.rs`
- Modify: `userspace/shell/src/main.rs` (mod declaration)
- Modify: `userspace/shell/src/commands.rs` (executor uses lookup)

- [ ] **Step 1: Create path_lookup.rs**

```rust
//! PATH-based binary resolution for the shell.
//!
//! When the user types `cat foo` (no `spawn` prefix, no absolute path),
//! the shell walks $PATH left-to-right looking for a container image
//! whose name matches `cat`. First hit wins.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libcluu::fs::client::VfsClient;

/// Try to resolve a bare command name to a container path by walking PATH.
/// Returns Some(canonical_name) if found, None otherwise.
pub fn resolve(bare_name: &str, path_env: &str, vfs: &VfsClient) -> Option<String> {
    if bare_name.contains('/') {
        // Already absolute or has a slash — caller should handle.
        return None;
    }
    for dir in path_env.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = format!("{}/{}", dir.trim_end_matches('/'), bare_name);
        // Existence check: stat the canonical path. /var/images/<name>/manifest.toml
        // for now (mirroring procmgr's lookup).
        let manifest_path = format!("/var/images/{}/manifest.toml", bare_name);
        if vfs.stat(&manifest_path).is_ok() {
            return Some(bare_name.to_string());  // Container name, not path
        }
    }
    None
}
```

- [ ] **Step 2: Wire module into main.rs**

```rust
mod path_lookup;
```

- [ ] **Step 3: Hook executor to use path_lookup**

In `commands.rs`'s executor (the place that today gives "command not found" or requires `spawn` keyword), after builtin lookup fails:

```rust
let path_env = context.get("PATH").unwrap_or("/bin:/usr/bin").to_string();
let vfs = /* obtain VfsClient */;
if let Some(resolved) = crate::path_lookup::resolve(&name, &path_env, &vfs) {
    return spawn_process_with_argv(context, &resolved, DEFAULT_PRIORITY, &arg_refs);
}
// Otherwise: not found.
let line = format!("shell: {}: command not found\n", name);
send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
context.set_last_status(127);
Ok(ExecResult::Handled)
```

- [ ] **Step 4: Add l2_bare_cmd harness case**

```
l2_bare_cmd|full|MARKER_MODE=l2_bare_cmd TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

```bash
l2_bare_cmd)
    TEST_COMMAND=""
    SHELL_AUTOSTART_CMD_DEFAULT="cat /etc/motd"   # NO spawn prefix
    ;;
```

```bash
l2_bare_cmd)
    required_markers=(
        "TSC calibrated"
        "[USER] shell: ready"
        "Welcome to CLUU"
    )
    ;;
```

- [ ] **Step 5: Run case**

Run: `scripts/harness_run.sh l2_bare_cmd`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add userspace/shell/src/path_lookup.rs userspace/shell/src/main.rs userspace/shell/src/commands.rs scripts/harness_*
git commit -m "shell: PATH-based bare-command resolution; l2_bare_cmd green"
```

---

## Phase 6 — shellrc sourcing + env mirror

### Task 18: shellrc.rs file source executor

**Files:**
- Create: `userspace/shell/src/shellrc.rs`
- Modify: `userspace/shell/src/main.rs` (mod declaration)

- [ ] **Step 1: Create shellrc.rs**

```rust
//! shellrc — source a shell rc file (sequence of shell commands).
//!
//! Used at startup to load /etc/shellrc and ~/.shellrc. Each non-empty
//! non-comment line is fed through the existing executor.

use alloc::string::String;
use libcluu::fs::client::VfsClient;

use crate::commands::{BuiltinRegistry, CommandContext, CommandExecutor};

/// Try to read and execute a shellrc file. Missing file is silently
/// skipped (return Ok). Any per-line errors are logged via debug_print
/// but don't abort the source operation.
pub fn source_file(
    path: &str,
    stdout: usize,
    context: &mut CommandContext,
    registry: &BuiltinRegistry,
    vfs: &VfsClient,
) -> Result<(), libcluu::Error> {
    let file = match vfs.open(path) {
        Ok(f) => f,
        Err(_) => {
            // Missing file is fine.
            let _ = libcluu::debug_print(&alloc::format!("shellrc: {} not found, skipping", path));
            return Ok(());
        }
    };

    // Read whole file.
    let mut buf = alloc::vec::Vec::with_capacity(file.size);
    // ... use vfs.read_grant or similar pattern (see /bin/cat for reference) ...

    let _ = vfs.close(file);

    let text = match core::str::from_utf8(&buf) {
        Ok(s) => s,
        Err(_) => {
            let _ = libcluu::debug_print("shellrc: not UTF-8, skipping");
            return Ok(());
        }
    };

    // Execute each non-comment, non-empty line.
    for (lineno, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Reuse existing parser + executor.
        match cluu_lang::parse(trimmed) {
            Ok(program) => {
                let _ = registry.execute(stdout, context, &program);
            }
            Err(e) => {
                let _ = libcluu::debug_print(&alloc::format!(
                    "shellrc: {}:{} parse error: {:?}",
                    path, lineno + 1, e
                ));
            }
        }
    }
    Ok(())
}
```

(Adapt `cluu_lang::parse` and `registry.execute` to whatever the actual APIs are.)

- [ ] **Step 2: Hook module**

In `userspace/shell/src/main.rs`:
```rust
mod shellrc;
```

- [ ] **Step 3: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add userspace/shell/src/shellrc.rs userspace/shell/src/main.rs
git commit -m "shell/shellrc: file source executor (silent skip on missing)"
```

---

### Task 19: Source /etc/shellrc and ~/.shellrc at startup

**Files:**
- Modify: `userspace/shell/src/main.rs` (startup sequence)

- [ ] **Step 1: Add sourcing calls before REPL**

In the shell's startup sequence (after `print_prompt` but before the read-loop), insert:

```rust
let vfs = /* construct VfsClient — reuse pattern from existing cat/ls path */;

// Source /etc/shellrc first (system-wide).
let _ = crate::shellrc::source_file("/etc/shellrc", stdout, &mut context, &registry, &vfs);

// Then source ~/.shellrc (user-personal). HOME comes from envelope.
if let Some(home) = std::env::var_os("HOME") {  // or extern "C" environ pattern
    let user_rc = format!("{}/.shellrc", home.to_string_lossy());
    let _ = crate::shellrc::source_file(&user_rc, stdout, &mut context, &registry, &vfs);
}
```

(Adapt `std::env::var_os` to whatever no_std equivalent exists in libcluu.)

- [ ] **Step 2: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add userspace/shell/src/main.rs
git commit -m "shell: source /etc/shellrc and ~/.shellrc at startup"
```

---

### Task 20: Stage shellrc files into userdisk

**Files:**
- Create: `etc/shellrc`
- Create: `home/balazs/.shellrc`
- Modify: `xtask/src/main.rs` (userdisk staging)

- [ ] **Step 1: Create system shellrc**

```sh
# /etc/shellrc — system-wide shell startup.
# Loaded by /bin/shell before ~/.shellrc.

# All defaults already come from /etc/envelopes.toml; this file is
# a placeholder for system administrators to add or override.
```

- [ ] **Step 2: Create sample user shellrc**

```sh
# ~/.shellrc — personal shell startup for balazs.

export PATH=/bin:/usr/bin:$HOME/bin
cd $HOME
```

- [ ] **Step 3: Add to xtask staging**

Find the existing /etc-staging block and add lines for `/etc/shellrc` and `/home/balazs/.shellrc`.

- [ ] **Step 4: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 5: Add l2_shellrc harness case**

```
l2_shellrc|full|MARKER_MODE=l2_shellrc TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

```bash
l2_shellrc)
    TEST_COMMAND=""
    SHELL_AUTOSTART_CMD_DEFAULT="spawn envprobe HOME PATH"
    ;;
```

```bash
l2_shellrc)
    required_markers=(
        "TSC calibrated"
        "[USER] shell: ready"
        "envprobe: HOME=/home/balazs"
        "envprobe: PATH=/bin:/usr/bin:/home/balazs/bin"   # confirms ~/.shellrc ran
    )
    ;;
```

- [ ] **Step 6: Run case**

Run: `scripts/harness_run.sh l2_shellrc`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add etc/shellrc home/balazs/.shellrc xtask/src/main.rs scripts/harness_*
git commit -m "shellrc: ship /etc/shellrc + sample home/balazs/.shellrc; l2_shellrc green"
```

---

## Phase 7 — MicroPython acceptance + envelope hardening

### Task 21: Verify mp can read /etc/motd

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Add l2_mp_etc harness case**

```
l2_mp_etc|full|MARKER_MODE=l2_mp_etc TEST_COMMAND_REPEAT=1 RUN_WAIT=30
```

```bash
l2_mp_etc)
    TEST_COMMAND=""
    SHELL_AUTOSTART_CMD_DEFAULT='spawn mp -c "open(chr(47)+chr(101)+chr(116)+chr(99)+chr(47)+chr(109)+chr(111)+chr(116)+chr(100)).read()"'
    ;;
```

```bash
l2_mp_etc)
    required_markers=(
        "TSC calibrated"
        "[USER] shell: ready"
        "procmgr: exit cookie 6 (code 0)"   # mp succeeds reading /etc/motd
    )
    ;;
```

- [ ] **Step 2: Run case**

Run: `scripts/harness_run.sh l2_mp_etc`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add scripts/harness_*
git commit -m "harness: l2_mp_etc — MicroPython reads /etc/motd via envelope-mounted /etc"
```

---

### Task 22: Revert mp debug instrumentation

**Files:**
- Modify: `userspace/micropython/main.c`

- [ ] **Step 1: Remove the debug_print bookends**

Open `userspace/micropython/main.c`. Remove the diagnostic `debug_print` calls in `do_str` and the VFS mount block, and the `extern void debug_print` declaration. Restore the file to its pre-instrumentation state (matching the version before commit `eb92470`'s debug additions; if eb92470 is the one that added them, revert just that scope).

- [ ] **Step 2: Build**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 3: Re-run l2_mp_etc**

Run: `scripts/harness_run.sh l2_mp_etc`
Expected: PASS (mp still works without the diagnostics).

- [ ] **Step 4: Commit**

```bash
git add userspace/micropython/main.c
git commit -m "micropython: remove debug_print instrumentation (no longer needed)"
```

---

## Phase 8 — Final regression + docs

### Task 23: Full harness matrix sweep

**Files:** none (verification only)

- [ ] **Step 1: Run full matrix**

Run: `scripts/harness_matrix.sh`
Expected: All previously green cases stay green. New `l2_envelope_*`, `l2_cluufile_*`, `l2_bare_cmd`, `l2_export`, `l2_shellrc`, `l2_mp_etc` all green.

Pre-existing flakes (`l2_argv`, `l2_owner_deny`, `l2_fg`, `p2_pipe`, `p2_spawn_pipe`, `p4_dev`) — known suite-only fails per task #78. Acceptable.

- [ ] **Step 2: Commit nothing**

If all pass, the work is verified. No further code changes.

If a regression appears that wasn't there pre-spec: bisect against the last commit before this plan started (`eb92470` if mp instrumentation was the last thing on develop, otherwise `e73f0e6` which is the spec commit).

---

### Task 24: Update CURRENT_PHASE.md and memory

**Files:**
- Modify: `~/cluu-notes/CURRENT_PHASE.md`
- Modify: `~/.claude/projects/-home-vlb2bp-git-cluu/memory/MEMORY.md` and `project_micropython_diagnostic.md`

- [ ] **Step 1: Tick Phase 2 first criterion fully done**

In `cluu-notes/CURRENT_PHASE.md`, update the Phase 2 status: MicroPython runs a one-liner AND reads a file from disk are now both ✓.

- [ ] **Step 2: Add memory entry**

Create `~/.claude/projects/-home-vlb2bp-git-cluu/memory/project_envelope.md` summarizing the envelope architecture for future sessions: where envelopes.toml lives, how session-login resolves, strict Cluufile semantics, shellrc loading order. ~30 lines.

- [ ] **Step 3: Update MEMORY.md index**

Add a one-line entry pointing to the new memory file.

- [ ] **Step 4: Commit notes**

```bash
# (not in repo — these are personal notes)
```

(Skip git commit; these files live outside the repo.)

---

## Acceptance criteria (mirror of spec §11)

- [ ] `/etc/envelopes.toml` ships with admin/user/service envelopes.
- [ ] Procmgr parses envelopes.toml at boot; panics on malformed.
- [ ] Session-login resolves user.profile → envelope; failed lookup rejects login.
- [ ] Shell sources `/etc/shellrc` then `~/.shellrc` at startup (silent skip on missing).
- [ ] Bare-command resolution walks `$PATH`. `cat /etc/motd` works.
- [ ] `export FOO=bar` propagates to children; `set FOO=bar` doesn't.
- [ ] Cluufile MOUNT consistent with envelope succeeds; mismatches fail spawn with clear error.
- [ ] Shell's `vars ∩ exported` mirrors to newlib `_environ`.
- [ ] `spawn mp -c "open('/etc/motd').read()"` exits with code 0.
- [ ] Full harness matrix stays green (pre-existing flakes excepted).

---

## Out of scope (per spec §12)

- Per-user envelope overrides (post-v1).
- Configurable shellrc paths.
- Bidirectional env mirror.
- `mounts_private` / `mounts_deny` envelope modes.
- Aliases / shell functions.
- `PS1` / `PS2` prompt customization.
- `env -i` / `env FOO=bar cmd` builtins.
- TOML build-time validation.

---

## Notes on task granularity

Tasks 5, 9, 13, 16, 18 span multiple files but are still single logical commits — splitting them would force broken-build commits. If a subagent finds a task running over 60 minutes, decompose on the fly into sub-commits (e.g., Task 13 could split into "validate function" + "hook into handler").

Tasks 6 and 17 are the riskiest. Task 6 changes the VFS_SET_VIEW wire format which all existing clients depend on; verify each spawn site still works. Task 17 hooks into the shell's executor at a junction where many code paths converge.
