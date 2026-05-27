# procmgr Cap-Model Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split current single 7,618-line procmgr into `root-procmgr` (system-scope, primordial) + per-session `session-procmgr` instances, deleting all runtime identity checks and replacing them with structural cap-derivation per CLUU's possession-equals-authority model.

**Architecture:** Hierarchical multi-instance, Genode-`init`-style. `root-procmgr` mints session-scoped caps; each `session-procmgr` sub-mints child-scoped caps; cap derivation is monotone-narrowing. PIDs encode `(8-bit session_id | 23-bit local pid)`. Cascade teardown on session-procmgr death via cap revocation. Three crates: `procmgr-common` (lib), `root-procmgr` (bin), `session-procmgr` (bin).

**Tech Stack:** Rust `no_std` + `alloc`, `spin::Mutex`, `postcard` IPC, `libcluu::syscall` for kernel surface, `cluu_wire` for wire types, `cargo llvm-cov` for coverage, `proptest` for property tests.

**Spec:** [`docs/superpowers/specs/2026-05-21-procmgr-cap-refactor-design.md`](../specs/2026-05-21-procmgr-cap-refactor-design.md)

---

## Status — 2026-05-26 (post-audit)

**Net:** 37/40 tasks LANDED on branch `procmgr-cap-refactor`. Plan body checkboxes
never ticked during execution; ground truth = code. Audit details below.

| Phase | Status | Notes |
|-------|--------|-------|
| 0 (scaffold) | ✅ LANDED | All 3 crates exist (`userspace/libs/procmgr-common`, `userspace/root-procmgr`, `userspace/session-procmgr`) |
| 1 (procmgr-common types) | ✅ LANDED | pid/labels/wire/handler/mint_guard + envelopes/manifest_cache/mount_policy/view_table ported. 35 unit tests pass. |
| 2 (root-procmgr skeleton + MintGuard) | ✅ LANDED | dispatch.rs + mint_guard RAII |
| 3 (SessionDirectory + Create/Destroy) | ✅ LANDED | session_directory.rs + handlers + proptest |
| 4 (cap_broker) | ✅ LANDED | sub_mint + monotone-narrowing proptest |
| 5 (session-procmgr spawn) | ✅ LANDED | ChildTable + spawn.rs + rollback guard |
| 6 (child_monitor) | ✅ LANDED | exit handler + restart policy SM |
| 7 (kill/pg_table/ctty) | ✅ LANDED | All 3 handlers in session-procmgr |
| 8 (pipe_registry) | ✅ LANDED | pipe_registry.rs + handlers |
| 9 (proc_query_local/all) | ✅ LANDED | local in session-procmgr, all in root-procmgr |
| 10 (services + restart) | ✅ LANDED | services.rs + restart_root.rs |
| 11 (escalate + shutdown) | ✅ LANDED | escalate.rs + shutdown.rs |
| 12 (bootstrap rewire) | ✅ LANDED | real_kernel.rs in both crates; login → SESSION_CREATE; legacy bypass deleted in 6d2bf44 (2026-05-26) |
| 13.1 (xtask check-cap-purity) | ✅ LANDED | `xtask check-cap-purity` grep gate (commit 70739a4) |
| 13.2 (pm_* integration tests) | ⚠️ PARTIAL | Only `pm_vfs_view_scope` exists. Plan calls for 7: pm_pid_layout, pm_session_create_destroy, pm_cap_monotone, pm_spawn_rollback, pm_kill_cascade, pm_restart_policy, pm_view_scope |
| 14.1–14.4 (coverage/perf/merge) | ❌ PENDING | llvm-cov gate, coverage matrix doc, perf ratchet, final acceptance |

**Test summary:** 83 unit tests pass (procmgr-common 35, root-procmgr 28, session-procmgr 35). No ACL-style runtime checks left in code (per audit + commit 70739a4 grep gate).

**To merge to develop:** complete 13.2 missing tests (6 of 7) + decide whether 14.1/14.2/14.3 are merge blockers or post-merge polish. 14.4 (acceptance gate) reads ratchet + coverage outputs.

---

## Conventions for this Plan

- **TDD strict.** Every behavior is: failing test → minimal impl → green test → commit.
- **Each commit must build the entire workspace.** Run `cargo xtask build` after each commit step. Failures are blockers, not skipped.
- **Branch:** all work on `procmgr-cap-refactor` (created in Phase 0). Never merge to `develop` until Phase 14 acceptance.
- **No `#[allow(...)]`** without comment justifying why.
- **Commit messages:** conventional commits format (`feat:`, `refactor:`, `test:`, `chore:`).

---

## Phase 0 — Branch & Workspace Scaffold

### Task 0.1: Create refactor branch

**Files:** none (git operation only).

- [ ] **Step 1: Stash any uncommitted work and switch off legacy WIP**

```bash
git status
git stash push -m "pre-procmgr-refactor stash"
```

Expected: status reports the eight modified userspace files; stash succeeds.

- [ ] **Step 2: Create and switch to refactor branch**

```bash
git checkout -b procmgr-cap-refactor
git log -1 --oneline
```

Expected: branch created, HEAD points at `3ef3028` (the spec commit) or later.

- [ ] **Step 3: Push branch upstream (so progress is visible)**

```bash
git push -u origin procmgr-cap-refactor
```

Expected: branch published.

### Task 0.2: Create `procmgr-common` crate skeleton

**Files:**
- Create: `userspace/libs/procmgr-common/Cargo.toml`
- Create: `userspace/libs/procmgr-common/src/lib.rs`
- Modify: `Cargo.toml` (workspace `members` + `default-members`)

- [ ] **Step 1: Write `userspace/libs/procmgr-common/Cargo.toml`**

```toml
[package]
name = "procmgr-common"
version = "0.1.0"
edition = "2021"
description = "Shared library for root-procmgr and session-procmgr"
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[dependencies]
libcluu = { path = "../../libcluu" }
cluu_wire = { path = "../../cluu_wire" }
spin = { workspace = true }
postcard = { workspace = true }
serde = { workspace = true, features = ["derive"] }

[dev-dependencies]
libcluu = { path = "../../libcluu", features = ["host-test"] }
proptest = "1"

[features]
default = []
host-test = ["libcluu/host-test"]

[lib]
name = "procmgr_common"
path = "src/lib.rs"
```

- [ ] **Step 2: Write `userspace/libs/procmgr-common/src/lib.rs`**

```rust
//! Shared library for root-procmgr and session-procmgr.
//!
//! Contains:
//! - Wire types (`wire`)
//! - IPC label constants (`labels`)
//! - PID encode/decode (`pid`)
//! - Handler dispatch trait (`handler`)
//! - Mock kernel surface for tests (`test_kernel`, `#[cfg(test)]`)
//! - Static envelope/manifest/mount/view utilities ported from legacy procmgr

#![cfg_attr(not(feature = "host-test"), no_std)]

extern crate alloc;

pub mod labels;
pub mod pid;
pub mod handler;
pub mod wire;

#[cfg(any(test, feature = "host-test"))]
pub mod test_kernel;
```

- [ ] **Step 3: Add crate to workspace `Cargo.toml`**

In the top-level `Cargo.toml`, add `"userspace/libs/procmgr-common"` to both `members` and `default-members` (alphabetical position, near `userspace/libcluu`).

- [ ] **Step 4: Build empty crate**

```bash
cargo build -p procmgr-common
```

Expected: PASS (empty modules compile).

- [ ] **Step 5: Commit**

```bash
git add userspace/libs/procmgr-common Cargo.toml
git commit -m "chore(procmgr-common): scaffold shared library crate"
```

### Task 0.3: Create `session-procmgr` crate skeleton

**Files:**
- Create: `userspace/session-procmgr/Cargo.toml`
- Create: `userspace/session-procmgr/src/main.rs`
- Create: `userspace/session-procmgr/src/lib.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Write `userspace/session-procmgr/Cargo.toml`**

```toml
[package]
name = "cluu-session-procmgr"
version = "0.1.0"
edition = "2021"
description = "CLUU per-session process manager"
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[dependencies]
libcluu = { path = "../libcluu" }
cluu_wire = { path = "../cluu_wire" }
procmgr-common = { path = "../libs/procmgr-common" }
spin = { workspace = true }
postcard = { workspace = true }

[dev-dependencies]
libcluu = { path = "../libcluu", features = ["host-test"] }
procmgr-common = { path = "../libs/procmgr-common", features = ["host-test"] }
proptest = "1"

[features]
default = []
host-test = [
    "libcluu/host-test",
    "procmgr-common/host-test",
]

[lib]
name = "session_procmgr"
path = "src/lib.rs"

[[bin]]
name = "session-procmgr"
path = "src/main.rs"
```

- [ ] **Step 2: Write `userspace/session-procmgr/src/lib.rs`**

```rust
#![cfg_attr(not(feature = "host-test"), no_std)]
extern crate alloc;
```

- [ ] **Step 3: Write `userspace/session-procmgr/src/main.rs` (placeholder)**

```rust
#![cfg_attr(not(feature = "host-test"), no_std)]
#![cfg_attr(not(feature = "host-test"), no_main)]
extern crate alloc;

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    // Bootstrap implemented in later phases.
    0
}
```

- [ ] **Step 4: Add crate to workspace `Cargo.toml`** (`members` + `default-members`)

- [ ] **Step 5: Build**

```bash
cargo build -p cluu-session-procmgr
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add userspace/session-procmgr Cargo.toml
git commit -m "chore(session-procmgr): scaffold per-session procmgr binary"
```

### Task 0.4: Rename `procmgr` → `root-procmgr`

**Files:**
- Rename directory: `userspace/procmgr/` → `userspace/root-procmgr/`
- Modify: `userspace/root-procmgr/Cargo.toml` (`name`, `lib.name`, `[[bin]].name`)
- Modify: top-level `Cargo.toml`
- Modify: `xtask/src/main.rs` (any literal "procmgr" references that point to the binary)
- Modify: `userspace/init/src/main.rs` if it references `procmgr` by path string
- Modify: `Cluufile` entries

- [ ] **Step 1: Rename directory**

```bash
git mv userspace/procmgr userspace/root-procmgr
```

- [ ] **Step 2: Edit `userspace/root-procmgr/Cargo.toml`**

```toml
[package]
name = "cluu-root-procmgr"
...
[lib]
name = "root_procmgr"
...
[[bin]]
name = "root-procmgr"
```

- [ ] **Step 3: Update workspace `Cargo.toml`** — replace `userspace/procmgr` with `userspace/root-procmgr` in `members` and `default-members`.

- [ ] **Step 4: Find and update binary-name references**

```bash
grep -rn '"procmgr"' xtask/ userspace/init/ Cluufile* 2>/dev/null
```

Update every match to `"root-procmgr"`. Show diffs in commit message.

- [ ] **Step 5: Find and update crate-name references**

```bash
grep -rn 'cluu-procmgr\|use procmgr::' userspace/ xtask/ 2>/dev/null
```

Update `cluu-procmgr` → `cluu-root-procmgr`, `use procmgr::` → `use root_procmgr::`.

- [ ] **Step 6: Build full workspace**

```bash
cargo xtask build
```

Expected: PASS. Any build break means a rename was missed.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: rename procmgr → root-procmgr"
```

---

## Phase 1 — `procmgr-common`: shared types, PID, handler trait, mock kernel

### Task 1.1: PID encode/decode

**Files:**
- Create: `userspace/libs/procmgr-common/src/pid.rs`
- Modify: `userspace/libs/procmgr-common/src/lib.rs` (export)

- [ ] **Step 1: Write failing test for encode/decode roundtrip**

In `userspace/libs/procmgr-common/src/pid.rs`:

```rust
//! PID layout: 8-bit session id (high) | 23-bit local pid (low).
//! `pid_t` is `i32`; sign bit reserved; 31 usable bits.

pub type SessionId = u8;
pub type LocalPid = u32; // 23-bit effective
pub type Pid = i32;

pub const SID_BITS: u32 = 8;
pub const LOCAL_BITS: u32 = 23;
pub const LOCAL_MAX: u32 = (1u32 << LOCAL_BITS) - 1;

#[derive(Debug, PartialEq, Eq)]
pub enum PidError {
    LocalOutOfRange,
}

pub fn encode(sid: SessionId, local: LocalPid) -> Result<Pid, PidError> {
    if local > LOCAL_MAX {
        return Err(PidError::LocalOutOfRange);
    }
    Ok(((sid as i32) << LOCAL_BITS) | (local as i32))
}

pub fn decode(pid: Pid) -> (SessionId, LocalPid) {
    let local = (pid as u32) & LOCAL_MAX;
    let sid = ((pid as u32) >> LOCAL_BITS) as u8;
    (sid, local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip_smoke() {
        assert_eq!(encode(0, 0).unwrap(), 0);
        assert_eq!(encode(5, 1).unwrap(), 0x2800001);
        assert_eq!(encode(255, LOCAL_MAX).unwrap(), 0x7FFFFFFF);
        assert_eq!(decode(0x2800001), (5, 1));
    }

    #[test]
    fn encode_local_overflow_errors() {
        assert_eq!(encode(0, LOCAL_MAX + 1), Err(PidError::LocalOutOfRange));
    }
}
```

Add `pub mod pid;` to `lib.rs` (already declared in Task 0.2).

- [ ] **Step 2: Run failing test (will fail because file did not exist before)**

```bash
cargo test -p procmgr-common --features host-test pid::tests::encode_decode_roundtrip_smoke
```

Expected: this should now PASS (we wrote impl + test together — TDD purist would split, pragmatic skipping here because the impl is one line per function).

- [ ] **Step 3: Add property test for roundtrip**

In same file, under `#[cfg(test)]`:

```rust
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn prop_encode_decode_roundtrip(sid in 0u8..=255, local in 0u32..=LOCAL_MAX) {
            let pid = encode(sid, local).unwrap();
            let (s, l) = decode(pid);
            prop_assert_eq!(s, sid);
            prop_assert_eq!(l, local);
            prop_assert!(pid >= 0); // never negative (sign bit clear)
        }
    }
```

- [ ] **Step 4: Run all pid tests**

```bash
cargo test -p procmgr-common --features host-test pid
```

Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add userspace/libs/procmgr-common/src/pid.rs
git commit -m "feat(procmgr-common): add 8|23 PID encode/decode with property tests"
```

### Task 1.2: Label constants

**Files:**
- Create: `userspace/libs/procmgr-common/src/labels.rs`

- [ ] **Step 1: Extract all `PROCMGR_*_LABEL` constants from current `userspace/root-procmgr/src/main.rs`**

```bash
grep -n "const PROCMGR_.*_LABEL\|const.*_LABEL.*u32" userspace/root-procmgr/src/main.rs
```

- [ ] **Step 2: Write `userspace/libs/procmgr-common/src/labels.rs`**

Paste every `PROCMGR_*_LABEL` constant found. Add the new labels this refactor introduces:

```rust
//! IPC label constants for procmgr ⇄ client traffic.
//! Source of truth — both root-procmgr and session-procmgr import from here.

// === existing (ported from legacy procmgr) ===
pub const PROCMGR_FAULT_LABEL: u32 = 0xFA017;
pub const PROCMGR_EXIT_LABEL: u32 = /* paste actual value */;
pub const PROCMGR_SPAWN_LABEL: u32 = /* paste */;
// ... port all the rest ...

// === new (root-procmgr only) ===
pub const PROCMGR_SESSION_CREATE_LABEL: u32 = 0xA000;
pub const PROCMGR_SESSION_DESTROY_LABEL: u32 = 0xA001;
pub const PROCMGR_SERVICE_SPAWN_LABEL: u32 = 0xA002;
pub const PROCMGR_PROC_QUERY_ALL_LABEL: u32 = 0xA003;
pub const PROCMGR_ESCALATE_LABEL: u32 = 0xA004;
pub const PROCMGR_SHUTDOWN_LABEL: u32 = 0xA005;

// === new (session-procmgr only) ===
pub const SESSION_PROCMGR_SPAWN_LABEL: u32 = 0xB000;
pub const SESSION_PROCMGR_KILL_LABEL: u32 = 0xB001;
pub const SESSION_PROCMGR_WAIT_LABEL: u32 = 0xB002;
pub const SESSION_PROCMGR_PROC_QUERY_LOCAL_LABEL: u32 = 0xB003;
pub const SESSION_PROCMGR_PIPE_CREATE_LABEL: u32 = 0xB004;
pub const SESSION_PROCMGR_PIPE_CLOSE_LABEL: u32 = 0xB005;
pub const SESSION_PROCMGR_PG_CREATE_LABEL: u32 = 0xB006;
pub const SESSION_PROCMGR_PG_ATTACH_LABEL: u32 = 0xB007;
pub const SESSION_PROCMGR_PG_SIGNAL_LABEL: u32 = 0xB008;
pub const SESSION_PROCMGR_CTTY_QUERY_LABEL: u32 = 0xB009;
```

- [ ] **Step 3: Build**

```bash
cargo build -p procmgr-common
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add userspace/libs/procmgr-common/src/labels.rs
git commit -m "feat(procmgr-common): collect IPC label constants"
```

### Task 1.3: Wire types

**Files:**
- Create: `userspace/libs/procmgr-common/src/wire.rs`

- [ ] **Step 1: Write `wire.rs`**

```rust
//! IPC wire types serialised via postcard.
//! Keep payloads ≤ 4 KiB (matches kernel inline IPC limit).

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::pid::{Pid, SessionId};

/// Session lifetime envelope from root-procmgr → session-procmgr at spawn.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionEnvelope {
    pub sid: SessionId,
    pub generation: u32,
    pub user_name: String,
    pub profile: String,            // ProfileSpec serialised
    pub pid_base: i32,              // sid << 23
    /// Caps minted by root for this session (handles by name → token).
    pub caps: Vec<(String, u64)>,
    pub env_defaults: Vec<(String, String)>,
    pub view_spec: String,          // serialised view (mount table)
}

/// Spawn request (session-procmgr child spawn).
#[derive(Debug, Serialize, Deserialize)]
pub struct SpawnReq {
    pub image_path: String,
    pub argv: Vec<String>,
    pub envp: Vec<(String, String)>,
    pub cwd: String,
    pub fd_inherit: Vec<FdInheritEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FdInheritEntry {
    pub fd: i32,
    pub kind: FdKind,
    pub cap_token: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum FdKind {
    VfsFile,
    VfsPipe,
    Pts,
    Tty,
    Null,
    Zero,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SpawnReply {
    pub pid: Pid,
    pub cookie: u64,
}

/// Exit notification (crt0 → session-procmgr).
#[derive(Debug, Serialize, Deserialize)]
pub struct ExitNotif {
    pub cookie: u64,
    pub exit_code: i32,
}

/// Proc query local (root → session-procmgr).
#[derive(Debug, Serialize, Deserialize)]
pub struct ProcQueryLocalReq {
    /// Empty = all procs in this session.
    pub pids: Vec<Pid>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcQueryLocalReply {
    pub procs: Vec<ProcInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcInfo {
    pub pid: Pid,
    pub ppid: Pid,
    pub state: u8,
    pub command: String,
    pub argv0: String,
    pub start_ticks: u64,
}
```

- [ ] **Step 2: Add test for roundtrip**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use postcard::{from_bytes, to_allocvec};

    #[test]
    fn spawn_req_postcard_roundtrip() {
        let req = SpawnReq {
            image_path: "/bin/ls".into(),
            argv: vec!["ls".into(), "-l".into()],
            envp: vec![("PATH".into(), "/bin".into())],
            cwd: "/".into(),
            fd_inherit: vec![FdInheritEntry { fd: 0, kind: FdKind::Pts, cap_token: 42 }],
        };
        let bytes = to_allocvec(&req).unwrap();
        let back: SpawnReq = from_bytes(&bytes).unwrap();
        assert_eq!(back.image_path, "/bin/ls");
        assert_eq!(back.fd_inherit[0].cap_token, 42);
    }
}
```

- [ ] **Step 3: Build + test**

```bash
cargo test -p procmgr-common --features host-test wire
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add userspace/libs/procmgr-common/src/wire.rs
git commit -m "feat(procmgr-common): IPC wire types"
```

### Task 1.4: Mock kernel surface

**Files:**
- Create: `userspace/libs/procmgr-common/src/test_kernel.rs`

- [ ] **Step 1: Write trait + mock**

```rust
//! Test-only mock kernel surface. Production code wraps real `libcluu::syscall`;
//! tests inject a recording mock.

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelCall {
    Mint   { parent: u64, rights: u32, new_handle: u64 },
    Revoke { handle: u64 },
    SpawnThread { entry: u64, stack: u64, tid: u64 },
    SendMsg { dest: u64, label: u32, len: usize },
    Recv   { token: u64 },
}

pub trait Kernel {
    fn mint(&mut self, parent: u64, rights: u32) -> u64;
    fn revoke(&mut self, handle: u64);
    fn spawn_thread(&mut self, entry: u64, stack: u64) -> u64;
}

#[derive(Default)]
pub struct MockKernel {
    pub calls: Vec<KernelCall>,
    pub next_handle: u64,
    pub revoked: BTreeMap<u64, bool>,
}

impl MockKernel {
    pub fn new() -> Self {
        Self { calls: Vec::new(), next_handle: 0x1000, revoked: BTreeMap::new() }
    }
}

impl Kernel for MockKernel {
    fn mint(&mut self, parent: u64, rights: u32) -> u64 {
        let new_handle = self.next_handle;
        self.next_handle += 1;
        self.calls.push(KernelCall::Mint { parent, rights, new_handle });
        new_handle
    }
    fn revoke(&mut self, handle: u64) {
        self.calls.push(KernelCall::Revoke { handle });
        self.revoked.insert(handle, true);
    }
    fn spawn_thread(&mut self, entry: u64, stack: u64) -> u64 {
        let tid = self.next_handle;
        self.next_handle += 1;
        self.calls.push(KernelCall::SpawnThread { entry, stack, tid });
        tid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mock_records_mint() {
        let mut k = MockKernel::new();
        let h = k.mint(0xAA, 0xFF);
        assert_eq!(h, 0x1000);
        assert_eq!(k.calls.len(), 1);
    }
    #[test]
    fn mock_records_revoke() {
        let mut k = MockKernel::new();
        k.revoke(0x1000);
        assert!(k.revoked.contains_key(&0x1000));
    }
}
```

- [ ] **Step 2: Build + test**

```bash
cargo test -p procmgr-common --features host-test test_kernel
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add userspace/libs/procmgr-common/src/test_kernel.rs
git commit -m "feat(procmgr-common): mock kernel surface for unit tests"
```

### Task 1.5: Handler trait + dispatcher

**Files:**
- Create: `userspace/libs/procmgr-common/src/handler.rs`

- [ ] **Step 1: Write handler trait**

```rust
//! Handler dispatch trait. Each IPC handler is one type implementing `MsgHandler`.
//! Dispatcher = static `label → fn ptr` table. Future async migration: trait
//! method becomes `async fn`, dispatcher becomes executor poll.

extern crate alloc;
use alloc::vec::Vec;

#[derive(Debug)]
pub struct Reply {
    pub words: [usize; 6],
    pub payload: Vec<u8>,
    pub label: u32,
}

impl Reply {
    pub fn ok(label: u32) -> Self {
        Self { words: [0; 6], payload: Vec::new(), label }
    }
    pub fn with_word(mut self, idx: usize, val: usize) -> Self {
        self.words[idx] = val;
        self
    }
    pub fn with_payload(mut self, p: Vec<u8>) -> Self {
        self.payload = p;
        self
    }
}

#[derive(Debug)]
pub enum HandlerError {
    BadCap,
    BadLabel,
    BadPayload,
    Internal(&'static str),
    Eagain,
    NotFound,
}

pub struct InboundMsg<'a> {
    pub label: u32,
    pub words: [usize; 6],
    pub payload: &'a [u8],
    pub sender_tid: usize,
}

pub trait MsgHandler {
    const LABEL: u32;
    type State;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl MsgHandler for Echo {
        const LABEL: u32 = 0xE000;
        type State = ();
        fn handle(_: &mut (), msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
            Ok(Reply::ok(Self::LABEL).with_word(0, msg.words[0]))
        }
    }

    #[test]
    fn echo_handler() {
        let msg = InboundMsg { label: 0xE000, words: [42, 0, 0, 0, 0, 0], payload: &[], sender_tid: 1 };
        let r = Echo::handle(&mut (), &msg).unwrap();
        assert_eq!(r.words[0], 42);
        assert_eq!(r.label, 0xE000);
    }
}
```

- [ ] **Step 2: Build + test**

```bash
cargo test -p procmgr-common --features host-test handler
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add userspace/libs/procmgr-common/src/handler.rs
git commit -m "feat(procmgr-common): MsgHandler trait + Reply/InboundMsg types"
```

### Task 1.6: Port `envelopes`, `manifest_cache`, `mount_policy`, `view_table` to procmgr-common

**Files:**
- Move: `userspace/root-procmgr/src/envelopes.rs` → `userspace/libs/procmgr-common/src/envelopes.rs`
- Move: `userspace/root-procmgr/src/manifest_cache.rs` → `userspace/libs/procmgr-common/src/manifest_cache.rs`
- Move: `userspace/root-procmgr/src/mount_policy.rs` → `userspace/libs/procmgr-common/src/mount_policy.rs`
- Move: `userspace/root-procmgr/src/view_table.rs` → `userspace/libs/procmgr-common/src/view_table.rs`
- Modify: both crates' `lib.rs`/`main.rs` to re-export / re-import.

- [ ] **Step 1: Move files**

```bash
git mv userspace/root-procmgr/src/envelopes.rs userspace/libs/procmgr-common/src/envelopes.rs
git mv userspace/root-procmgr/src/manifest_cache.rs userspace/libs/procmgr-common/src/manifest_cache.rs
git mv userspace/root-procmgr/src/mount_policy.rs userspace/libs/procmgr-common/src/mount_policy.rs
git mv userspace/root-procmgr/src/view_table.rs userspace/libs/procmgr-common/src/view_table.rs
```

- [ ] **Step 2: Update `procmgr-common/src/lib.rs`** — add module declarations:

```rust
pub mod envelopes;
pub mod manifest_cache;
pub mod mount_policy;
pub mod view_table;
```

- [ ] **Step 3: Update `root-procmgr/src/main.rs`** — replace internal module declarations with imports from `procmgr_common`:

```rust
// Replace:
//   mod envelopes;
//   mod manifest_cache;
//   mod mount_policy;
//   mod view_table;
// With:
use procmgr_common::{envelopes, manifest_cache, mount_policy, view_table};
```

Update every internal reference (`crate::envelopes::X` → `envelopes::X`, etc.).

- [ ] **Step 4: Add `procmgr-common` to root-procmgr deps**

In `userspace/root-procmgr/Cargo.toml`:

```toml
[dependencies]
procmgr-common = { path = "../libs/procmgr-common" }
```

- [ ] **Step 5: Build**

```bash
cargo build -p cluu-root-procmgr -p procmgr-common
```

Expected: PASS. If imports broke, fix path strings (most likely `cluu_wire::session` paths inside moved files are still correct since cluu_wire is a top-level dep).

- [ ] **Step 6: Run existing tests**

```bash
cargo test -p procmgr-common --features host-test
cargo test -p cluu-root-procmgr --features host-test
```

Expected: PASS (only the modules we moved, plus anything in root-procmgr that already had tests).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: move envelopes/manifest_cache/mount_policy/view_table to procmgr-common"
```

---

## Phase 2 — `root-procmgr`: bootstrap skeleton, MintGuard, dispatcher

### Task 2.1: Strip legacy `main.rs` down to dispatcher skeleton

**Goal:** keep current behavior (don't break boot) but introduce dispatcher pattern + module skeletons. Legacy handler functions stay inline but get wrapped by `MsgHandler` implementations one phase at a time.

**Files:**
- Modify: `userspace/root-procmgr/src/main.rs`
- Create: `userspace/root-procmgr/src/dispatch.rs`

- [ ] **Step 1: Add `dispatch.rs` with static handler table**

```rust
//! Static label → handler table. New handlers register here as they migrate
//! out of the legacy inline impl in `main.rs`.

use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};

pub struct ProcmgrState; // grown in later phases

pub fn dispatch(state: &mut ProcmgrState, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
    match msg.label {
        // each migrated handler registered here, e.g.:
        // crate::session_directory::SessionCreate::LABEL =>
        //     crate::session_directory::SessionCreate::handle(state, msg),
        _ => Err(HandlerError::BadLabel),
    }
}
```

- [ ] **Step 2: Wire `dispatch` into existing recv loop**

In `main.rs`, find the existing `ipc_recv_any_with_sender` block (around line 1944). Before the legacy `if index == ...` ladder, add:

```rust
// Try new dispatcher first. If unhandled, fall through to legacy.
let inbound = procmgr_common::handler::InboundMsg {
    label: msg.tag.label,
    words: msg.words,
    payload: payload,
    sender_tid,
};
match dispatch::dispatch(&mut self.dispatch_state, &inbound) {
    Ok(reply) => { /* send reply, return Ok(()) */ }
    Err(procmgr_common::handler::HandlerError::BadLabel) => { /* fall through */ }
    Err(e) => { /* log + drop */ return Ok(()); }
}
```

(Concrete code: pull `self.dispatch_state` into the struct; initialise to `ProcmgrState` in `new()`.)

- [ ] **Step 3: Build full workspace**

```bash
cargo xtask build
```

Expected: PASS, identical legacy behavior (no new labels registered yet).

- [ ] **Step 4: Boot smoke test**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
```

Expected: smoke marker fires green. Confirms dispatcher wrapper didn't break legacy flow.

- [ ] **Step 5: Commit**

```bash
git add userspace/root-procmgr/src/dispatch.rs userspace/root-procmgr/src/main.rs
git commit -m "refactor(root-procmgr): introduce dispatcher skeleton"
```

### Task 2.2: `MintGuard` RAII

**Files:**
- Create: `userspace/libs/procmgr-common/src/mint_guard.rs`
- Modify: `procmgr-common/src/lib.rs` (export)

- [ ] **Step 1: Failing test**

```rust
//! RAII guard that revokes minted caps on drop unless explicitly `forget`-ed.
//! Used in spawn rollback: mint all required caps inside guard, then
//! `mem::forget(guard)` after thread successfully starts.

extern crate alloc;
use alloc::vec::Vec;
use crate::test_kernel::Kernel;

pub struct MintGuard<'k, K: Kernel> {
    kernel: &'k mut K,
    minted: Vec<u64>,
    armed: bool,
}

impl<'k, K: Kernel> MintGuard<'k, K> {
    pub fn new(kernel: &'k mut K) -> Self {
        Self { kernel, minted: Vec::new(), armed: true }
    }
    pub fn mint(&mut self, parent: u64, rights: u32) -> u64 {
        let h = self.kernel.mint(parent, rights);
        self.minted.push(h);
        h
    }
    /// Disarm: caller takes ownership of all minted handles.
    pub fn forget(mut self) -> Vec<u64> {
        self.armed = false;
        core::mem::take(&mut self.minted)
    }
}

impl<'k, K: Kernel> Drop for MintGuard<'k, K> {
    fn drop(&mut self) {
        if self.armed {
            for h in self.minted.drain(..) {
                self.kernel.revoke(h);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_kernel::{KernelCall, MockKernel};

    #[test]
    fn guard_revokes_on_drop_when_armed() {
        let mut k = MockKernel::new();
        {
            let mut g = MintGuard::new(&mut k);
            let _h1 = g.mint(0xAA, 0xFF);
            let _h2 = g.mint(0xBB, 0xFF);
        } // dropped
        let revokes: Vec<_> = k.calls.iter()
            .filter(|c| matches!(c, KernelCall::Revoke { .. }))
            .collect();
        assert_eq!(revokes.len(), 2, "both minted handles revoked on drop");
    }

    #[test]
    fn forget_disarms_no_revoke() {
        let mut k = MockKernel::new();
        let handles;
        {
            let mut g = MintGuard::new(&mut k);
            g.mint(0xAA, 0xFF);
            g.mint(0xBB, 0xFF);
            handles = g.forget();
        }
        assert_eq!(handles.len(), 2);
        let revokes: Vec<_> = k.calls.iter()
            .filter(|c| matches!(c, KernelCall::Revoke { .. }))
            .collect();
        assert_eq!(revokes.len(), 0, "forget disarms guard");
    }
}
```

- [ ] **Step 2: Export from lib.rs**

```rust
pub mod mint_guard;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p procmgr-common --features host-test mint_guard
```

Expected: PASS (2 tests).

- [ ] **Step 4: Commit**

```bash
git add userspace/libs/procmgr-common/src/mint_guard.rs userspace/libs/procmgr-common/src/lib.rs
git commit -m "feat(procmgr-common): MintGuard RAII for spawn-rollback safety"
```

---

## Phase 3 — `root-procmgr`: `session_directory` (sid alloc, generation, create/destroy)

### Task 3.1: SessionEntry + SessionDirectory struct

**Files:**
- Create: `userspace/root-procmgr/src/session_directory.rs`

- [ ] **Step 1: Failing test for fresh sid allocation**

```rust
//! Per-session bookkeeping owned by root-procmgr.
//! Holds session_id allocator (8-bit + generation counter), session metadata,
//! and the spawn endpoint to talk to each session-procmgr instance.

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use procmgr_common::pid::SessionId;

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub sid: SessionId,
    pub generation: u32,
    pub user_name: String,
    pub session_pmgr_thread_tok: u64,
    pub session_pmgr_spawn_ep: u64,
    pub minted_caps: Vec<u64>,      // every cap root minted for this session
    pub state: SessionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState { Live, Dying, Dead }

pub struct SessionDirectory {
    /// generations[sid] = next generation to use when reallocating that sid.
    generations: [u32; 256],
    sessions: BTreeMap<SessionId, SessionEntry>,
    free_stack: Vec<SessionId>,
    next_fresh: u16,                // 16-bit to overflow past 255 = no fresh
}

#[derive(Debug, PartialEq, Eq)]
pub enum DirError { Exhausted, NotFound, AlreadyDead }

impl SessionDirectory {
    pub fn new() -> Self {
        let mut free = Vec::new();
        for i in (0..=255u8).rev() { free.push(i); }
        Self {
            generations: [0; 256],
            sessions: BTreeMap::new(),
            free_stack: free,
            next_fresh: 0,
        }
    }

    pub fn alloc_sid(&mut self) -> Result<(SessionId, u32), DirError> {
        let sid = self.free_stack.pop().ok_or(DirError::Exhausted)?;
        let gen = self.generations[sid as usize];
        Ok((sid, gen))
    }

    pub fn insert(&mut self, entry: SessionEntry) {
        self.sessions.insert(entry.sid, entry);
    }

    pub fn lookup(&self, sid: SessionId) -> Option<&SessionEntry> {
        self.sessions.get(&sid)
    }

    pub fn mark_dying(&mut self, sid: SessionId) -> Result<(), DirError> {
        let entry = self.sessions.get_mut(&sid).ok_or(DirError::NotFound)?;
        if entry.state == SessionState::Dead {
            return Err(DirError::AlreadyDead);
        }
        entry.state = SessionState::Dying;
        Ok(())
    }

    /// Mark dead, bump generation, free sid for reuse.
    pub fn finalise_dead(&mut self, sid: SessionId) -> Result<Vec<u64>, DirError> {
        let entry = self.sessions.remove(&sid).ok_or(DirError::NotFound)?;
        // Bump generation (wrap is fine; combined with sid forms unique cap-id space).
        self.generations[sid as usize] = self.generations[sid as usize].wrapping_add(1);
        self.free_stack.push(sid);
        Ok(entry.minted_caps)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SessionEntry> {
        self.sessions.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sid: SessionId, gen: u32) -> SessionEntry {
        SessionEntry {
            sid, generation: gen,
            user_name: "alice".into(),
            session_pmgr_thread_tok: 0x100,
            session_pmgr_spawn_ep: 0x200,
            minted_caps: vec![0xA, 0xB, 0xC],
            state: SessionState::Live,
        }
    }

    #[test]
    fn alloc_sid_starts_at_zero_or_predictable() {
        let mut d = SessionDirectory::new();
        let (s, g) = d.alloc_sid().unwrap();
        assert_eq!(g, 0);
        assert!((0..=255u8).contains(&s));
    }

    #[test]
    fn destroy_bumps_generation_and_returns_caps() {
        let mut d = SessionDirectory::new();
        let (s, g) = d.alloc_sid().unwrap();
        d.insert(entry(s, g));
        d.mark_dying(s).unwrap();
        let caps = d.finalise_dead(s).unwrap();
        assert_eq!(caps, vec![0xA, 0xB, 0xC]);
        // realloc — must yield generation = g+1
        let (s2, g2) = d.alloc_sid().unwrap();
        if s2 == s {
            assert_eq!(g2, g.wrapping_add(1));
        } else {
            // free stack is LIFO; ensure recycling eventually happens
            for _ in 0..255 {
                let _ = d.alloc_sid();
            }
        }
    }

    #[test]
    fn exhaustion_returns_err() {
        let mut d = SessionDirectory::new();
        for _ in 0..256 { d.alloc_sid().unwrap(); }
        assert_eq!(d.alloc_sid(), Err(DirError::Exhausted));
    }

    #[test]
    fn mark_dying_unknown_sid_errors() {
        let mut d = SessionDirectory::new();
        assert_eq!(d.mark_dying(42), Err(DirError::NotFound));
    }
}
```

Declare module in `root-procmgr/src/main.rs`:

```rust
mod session_directory;
```

- [ ] **Step 2: Run failing tests**

```bash
cargo test -p cluu-root-procmgr --features host-test session_directory
```

Expected: PASS (file already contains impl). If any test fails, fix impl until green.

- [ ] **Step 3: Add proptest for create/destroy uniqueness**

```rust
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn prop_no_duplicate_live_sids(ops in proptest::collection::vec(0u8..2, 1..100)) {
            let mut d = SessionDirectory::new();
            let mut held: Vec<(SessionId, u32)> = Vec::new();
            for op in ops {
                if op == 0 {
                    if let Ok((s, g)) = d.alloc_sid() {
                        // ensure sid not already held
                        prop_assert!(!held.iter().any(|(h, _)| *h == s));
                        d.insert(entry(s, g));
                        held.push((s, g));
                    }
                } else if let Some(idx) = held.iter().position(|_| true) {
                    let (s, _) = held.remove(idx);
                    d.mark_dying(s).unwrap();
                    d.finalise_dead(s).unwrap();
                }
            }
        }
    }
```

- [ ] **Step 4: Run all tests**

```bash
cargo test -p cluu-root-procmgr --features host-test session_directory
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add userspace/root-procmgr/src/session_directory.rs userspace/root-procmgr/src/main.rs
git commit -m "feat(root-procmgr): SessionDirectory with sid alloc + generation counter"
```

### Task 3.2: `SessionCreate` handler

**Files:**
- Modify: `userspace/root-procmgr/src/session_directory.rs` (add handler struct)
- Modify: `userspace/root-procmgr/src/dispatch.rs` (register handler)

- [ ] **Step 1: Failing test**

In `session_directory.rs`:

```rust
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_SESSION_CREATE_LABEL;
use procmgr_common::wire::SessionEnvelope;

pub struct SessionCreate;

impl MsgHandler for SessionCreate {
    const LABEL: u32 = PROCMGR_SESSION_CREATE_LABEL;
    type State = crate::dispatch::ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        // Decode request from payload.
        let req: SessionCreateReq = postcard::from_bytes(msg.payload)
            .map_err(|_| HandlerError::BadPayload)?;
        // Alloc sid.
        let (sid, gen) = state.session_directory.alloc_sid()
            .map_err(|_| HandlerError::Eagain)?;
        // Build envelope: caps minted in Phase 4 (cap_broker). For now,
        // empty caps; cap_broker integration in Task 4.2.
        let envelope = SessionEnvelope {
            sid, generation: gen,
            user_name: req.user_name.clone(),
            profile: req.profile,
            pid_base: ((sid as i32) << procmgr_common::pid::LOCAL_BITS),
            caps: Vec::new(),
            env_defaults: req.env_defaults,
            view_spec: req.view_spec,
        };
        // Spawn session-procmgr binary (stubbed for now — Task 5.1 implements).
        let (pmgr_tid, pmgr_ep) = (0, 0); // placeholder
        state.session_directory.insert(SessionEntry {
            sid, generation: gen,
            user_name: req.user_name,
            session_pmgr_thread_tok: pmgr_tid,
            session_pmgr_spawn_ep: pmgr_ep,
            minted_caps: Vec::new(),
            state: SessionState::Live,
        });
        let bytes = postcard::to_allocvec(&envelope)
            .map_err(|_| HandlerError::Internal("postcard"))?;
        Ok(Reply::ok(Self::LABEL).with_payload(bytes))
    }
}

#[derive(serde::Deserialize)]
pub struct SessionCreateReq {
    pub user_name: alloc::string::String,
    pub profile: alloc::string::String,
    pub env_defaults: alloc::vec::Vec<(alloc::string::String, alloc::string::String)>,
    pub view_spec: alloc::string::String,
}

#[cfg(test)]
mod handler_tests {
    use super::*;
    use procmgr_common::handler::InboundMsg;

    #[test]
    fn create_returns_envelope_with_pid_base() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        let req = SessionCreateReq {
            user_name: "alice".into(),
            profile: "user".into(),
            env_defaults: vec![],
            view_spec: "default".into(),
        };
        let payload = postcard::to_allocvec(&req).unwrap();
        let msg = InboundMsg {
            label: SessionCreate::LABEL,
            words: [0; 6],
            payload: &payload,
            sender_tid: 1,
        };
        let reply = SessionCreate::handle(&mut state, &msg).unwrap();
        let env: SessionEnvelope = postcard::from_bytes(&reply.payload).unwrap();
        assert_eq!(env.user_name, "alice");
        assert_eq!(env.generation, 0);
        assert_eq!(env.pid_base, ((env.sid as i32) << 23));
    }

    #[test]
    fn create_bad_payload_returns_badpayload() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        let msg = InboundMsg {
            label: SessionCreate::LABEL,
            words: [0; 6],
            payload: &[0xFF, 0xFF],
            sender_tid: 1,
        };
        assert!(matches!(SessionCreate::handle(&mut state, &msg), Err(HandlerError::BadPayload)));
    }

    #[test]
    fn create_exhausted_returns_eagain() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        for _ in 0..256 { state.session_directory.alloc_sid().unwrap(); }
        let req = SessionCreateReq { user_name: "alice".into(), profile: "u".into(),
            env_defaults: vec![], view_spec: "default".into() };
        let payload = postcard::to_allocvec(&req).unwrap();
        let msg = InboundMsg { label: SessionCreate::LABEL, words: [0; 6],
            payload: &payload, sender_tid: 1 };
        assert!(matches!(SessionCreate::handle(&mut state, &msg), Err(HandlerError::Eagain)));
    }
}
```

In `dispatch.rs`, add to `ProcmgrState`:

```rust
pub struct ProcmgrState {
    pub session_directory: crate::session_directory::SessionDirectory,
}

impl ProcmgrState {
    pub fn new() -> Self { Self { session_directory: Default::default() } }
    #[cfg(test)]
    pub fn new_for_test() -> Self { Self::new() }
}
```

And register the handler in the dispatcher match arm.

- [ ] **Step 2: Run failing tests** (some pass; the one checking sid pre-exhaustion is the new branch).

```bash
cargo test -p cluu-root-procmgr --features host-test session_directory::handler_tests
```

Expected: PASS (3 tests).

- [ ] **Step 3: Build**

```bash
cargo xtask build
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(root-procmgr): SessionCreate handler with branch tests"
```

### Task 3.3: `SessionDestroy` handler (cascade-teardown core)

**Files:**
- Modify: `userspace/root-procmgr/src/session_directory.rs`

- [ ] **Step 1: Failing tests for destroy invariants**

Add:

```rust
pub struct SessionDestroy;

impl MsgHandler for SessionDestroy {
    const LABEL: u32 = procmgr_common::labels::PROCMGR_SESSION_DESTROY_LABEL;
    type State = crate::dispatch::ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        // Caller presents session-destroy cap. Cap presence is authority.
        // Decode sid from words[0] (low byte).
        let sid = msg.words[0] as u8;
        // 1. Mark dying.
        state.session_directory.mark_dying(sid).map_err(|_| HandlerError::NotFound)?;
        // 2. Send SIGKILL to session-pmgr's children (deferred — Task 5.x covers).
        //    For now, just revoke caps.
        let caps = state.session_directory.finalise_dead(sid).map_err(|_| HandlerError::NotFound)?;
        for h in caps {
            // Real revoke: libcluu::syscall::cap_revoke(h). Mocked in tests via Kernel trait.
            state.kernel.revoke(h);
        }
        Ok(Reply::ok(Self::LABEL))
    }
}

#[cfg(test)]
mod destroy_tests {
    use super::*;
    use procmgr_common::test_kernel::{KernelCall, MockKernel};

    #[test]
    fn destroy_revokes_all_minted_caps() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        let (s, g) = state.session_directory.alloc_sid().unwrap();
        state.session_directory.insert(SessionEntry {
            sid: s, generation: g,
            user_name: "alice".into(),
            session_pmgr_thread_tok: 0x100,
            session_pmgr_spawn_ep: 0x200,
            minted_caps: vec![0xA, 0xB, 0xC],
            state: SessionState::Live,
        });
        let msg = InboundMsg {
            label: SessionDestroy::LABEL,
            words: [s as usize, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        SessionDestroy::handle(&mut state, &msg).unwrap();
        let revokes: Vec<u64> = state.kernel.calls.iter().filter_map(|c| match c {
            KernelCall::Revoke { handle } => Some(*handle), _ => None,
        }).collect();
        assert_eq!(revokes, vec![0xA, 0xB, 0xC]);
    }

    #[test]
    fn destroy_unknown_sid_returns_notfound() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        let msg = InboundMsg {
            label: SessionDestroy::LABEL,
            words: [99, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        assert!(matches!(SessionDestroy::handle(&mut state, &msg), Err(HandlerError::NotFound)));
    }

    #[test]
    fn destroy_bumps_generation_for_sid_reuse() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        let (s, g0) = state.session_directory.alloc_sid().unwrap();
        state.session_directory.insert(SessionEntry {
            sid: s, generation: g0, user_name: "u".into(),
            session_pmgr_thread_tok: 1, session_pmgr_spawn_ep: 2,
            minted_caps: vec![], state: SessionState::Live,
        });
        SessionDestroy::handle(&mut state, &InboundMsg {
            label: 0, words: [s as usize, 0, 0, 0, 0, 0], payload: &[], sender_tid: 1,
        }).unwrap();
        // The generations table internally bumped for slot `s`. New alloc of same slot must be g0+1.
        // (Allocation is LIFO, so the just-freed sid is the next pop.)
        let (s2, g1) = state.session_directory.alloc_sid().unwrap();
        assert_eq!(s2, s);
        assert_eq!(g1, g0.wrapping_add(1));
    }
}
```

`ProcmgrState` must hold a `Kernel` mock for tests:

```rust
pub struct ProcmgrState {
    pub session_directory: SessionDirectory,
    pub kernel: procmgr_common::test_kernel::MockKernel, // production: real impl
}
```

(Production binary will use a concrete `RealKernel` shim. Phase 5/12 will wire it. For now `cfg(test)` ok.)

- [ ] **Step 2: Run tests**

```bash
cargo test -p cluu-root-procmgr --features host-test session_directory::destroy_tests
```

Expected: PASS (3 tests).

- [ ] **Step 3: Register destroy in dispatcher.**

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(root-procmgr): SessionDestroy with cascade revoke + generation bump"
```

---

## Phase 4 — `root-procmgr`: `cap_broker` (sub-mint vfs/registry/timeserver per session)

### Task 4.1: `sub_mint` core + monotone invariant property test

**Files:**
- Create: `userspace/root-procmgr/src/cap_broker.rs`

- [ ] **Step 1: Failing tests for monotone narrowing**

```rust
//! Cap broker: root-procmgr mints session-scoped caps from its primordial
//! handles. Each mint narrows rights (monotone). MintGuard wraps the
//! sequence for rollback on partial failure.

extern crate alloc;
use alloc::vec::Vec;
use procmgr_common::mint_guard::MintGuard;
use procmgr_common::test_kernel::Kernel;

/// Bitmask of capability rights. Caller passes a subset of parent's rights.
#[derive(Debug, Clone, Copy)]
pub struct CapRights(pub u32);

pub fn sub_mint<K: Kernel>(
    guard: &mut MintGuard<'_, K>,
    parent: u64,
    parent_rights: CapRights,
    requested: CapRights,
) -> Result<u64, BrokerError> {
    if requested.0 & !parent_rights.0 != 0 {
        return Err(BrokerError::WiderThanParent);
    }
    Ok(guard.mint(parent, requested.0))
}

#[derive(Debug, PartialEq, Eq)]
pub enum BrokerError { WiderThanParent }

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::test_kernel::MockKernel;
    use proptest::prelude::*;

    #[test]
    fn narrowing_ok() {
        let mut k = MockKernel::new();
        let mut g = MintGuard::new(&mut k);
        let h = sub_mint(&mut g, 0xAAAA, CapRights(0xFF), CapRights(0x0F)).unwrap();
        let _ = g.forget();
        assert_eq!(h, 0x1000);
    }

    #[test]
    fn widening_fails() {
        let mut k = MockKernel::new();
        let mut g = MintGuard::new(&mut k);
        assert_eq!(
            sub_mint(&mut g, 0xAAAA, CapRights(0x0F), CapRights(0xFF)),
            Err(BrokerError::WiderThanParent)
        );
    }

    proptest! {
        #[test]
        fn prop_child_subset_of_parent(parent in 0u32..=u32::MAX, req in 0u32..=u32::MAX) {
            let mut k = MockKernel::new();
            let mut g = MintGuard::new(&mut k);
            let result = sub_mint(&mut g, 0xAAAA, CapRights(parent), CapRights(req));
            if (req & !parent) == 0 {
                prop_assert!(result.is_ok());
            } else {
                prop_assert!(result.is_err());
            }
            let _ = g.forget();
        }
    }
}
```

Declare module in `main.rs`.

- [ ] **Step 2: Run tests**

```bash
cargo test -p cluu-root-procmgr --features host-test cap_broker
```

Expected: PASS (3 tests including proptest).

- [ ] **Step 3: Commit**

```bash
git add userspace/root-procmgr/src/cap_broker.rs userspace/root-procmgr/src/main.rs
git commit -m "feat(root-procmgr): cap_broker::sub_mint with monotone proptest"
```

### Task 4.2: Integrate cap_broker into `SessionCreate`

**Files:**
- Modify: `userspace/root-procmgr/src/session_directory.rs`

- [ ] **Step 1: Failing test — SessionCreate populates envelope.caps**

Add to `handler_tests`:

```rust
    #[test]
    fn create_mints_vfs_registry_timeserver_caps() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        // seed root's parent handles for known services.
        state.parent_vfs_cap = 0xV000;
        state.parent_registry_cap = 0xR000;
        state.parent_timeserver_cap = 0xT000;
        // ... call handler ...
        let env = call_create(&mut state, "alice");
        assert!(env.caps.iter().any(|(n, _)| n == "vfs"));
        assert!(env.caps.iter().any(|(n, _)| n == "registry"));
        assert!(env.caps.iter().any(|(n, _)| n == "timeserver"));
        // ensure minted_caps recorded in directory for cascade revoke.
        let entry = state.session_directory.lookup(env.sid).unwrap();
        assert_eq!(entry.minted_caps.len(), 3);
    }
```

(`call_create` is a small helper that builds the payload + invokes the handler.)

- [ ] **Step 2: Update `SessionCreate::handle` to call cap_broker**

```rust
        let mut g = MintGuard::new(&mut state.kernel);
        let vfs_cap = crate::cap_broker::sub_mint(&mut g, state.parent_vfs_cap,
            CapRights(state.parent_vfs_rights), CapRights(VFS_SESSION_RIGHTS))
            .map_err(|_| HandlerError::Internal("vfs sub_mint"))?;
        let reg_cap = crate::cap_broker::sub_mint(&mut g, state.parent_registry_cap,
            CapRights(state.parent_registry_rights), CapRights(REGISTRY_SESSION_RIGHTS))
            .map_err(|_| HandlerError::Internal("reg sub_mint"))?;
        let ts_cap = crate::cap_broker::sub_mint(&mut g, state.parent_timeserver_cap,
            CapRights(state.parent_timeserver_rights), CapRights(TIMESERVER_SESSION_RIGHTS))
            .map_err(|_| HandlerError::Internal("ts sub_mint"))?;
        let minted_caps = g.forget();
        let envelope = SessionEnvelope {
            sid, generation: gen,
            user_name: req.user_name.clone(),
            profile: req.profile,
            pid_base: ((sid as i32) << 23),
            caps: vec![
                ("vfs".into(), vfs_cap),
                ("registry".into(), reg_cap),
                ("timeserver".into(), ts_cap),
            ],
            env_defaults: req.env_defaults,
            view_spec: req.view_spec,
        };
        state.session_directory.insert(SessionEntry {
            sid, generation: gen,
            user_name: req.user_name,
            session_pmgr_thread_tok: 0,
            session_pmgr_spawn_ep: 0,
            minted_caps,
            state: SessionState::Live,
        });
```

Constants:
```rust
pub const VFS_SESSION_RIGHTS: u32 = 0x07;          // read/write/open
pub const REGISTRY_SESSION_RIGHTS: u32 = 0x03;     // lookup/subscribe
pub const TIMESERVER_SESSION_RIGHTS: u32 = 0x01;   // read-only
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p cluu-root-procmgr --features host-test
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(root-procmgr): SessionCreate mints vfs/registry/timeserver via cap_broker"
```

---

## Phase 5 — `session-procmgr`: dispatcher, `spawn` handler, `child_table`

### Task 5.1: `session-procmgr` dispatcher + `ChildTable`

**Files:**
- Create: `userspace/session-procmgr/src/dispatch.rs`
- Create: `userspace/session-procmgr/src/child_table.rs`

- [ ] **Step 1: ChildTable with failing tests**

`userspace/session-procmgr/src/child_table.rs`:

```rust
extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use procmgr_common::pid::{LOCAL_MAX, LocalPid, Pid, SessionId};

#[derive(Debug, Clone)]
pub struct ChildState {
    pub pid: Pid,
    pub local: LocalPid,
    pub thread_tok: u64,
    pub cookie: u64,
    pub argv0: String,
    pub start_ticks: u64,
    pub minted_caps: Vec<u64>, // sub-mints for this child
    pub pgid: Option<u32>,
}

pub struct ChildTable {
    sid: SessionId,
    next_local: LocalPid,
    by_pid: BTreeMap<Pid, ChildState>,
    by_cookie: BTreeMap<u64, Pid>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChildTableError { Exhausted, NotFound }

impl ChildTable {
    pub fn new(sid: SessionId) -> Self {
        Self { sid, next_local: 1, by_pid: BTreeMap::new(), by_cookie: BTreeMap::new() }
    }

    pub fn alloc_pid(&mut self) -> Result<Pid, ChildTableError> {
        if self.next_local > LOCAL_MAX {
            return Err(ChildTableError::Exhausted);
        }
        let local = self.next_local;
        self.next_local += 1;
        Ok(procmgr_common::pid::encode(self.sid, local).unwrap())
    }

    pub fn insert(&mut self, child: ChildState) {
        self.by_cookie.insert(child.cookie, child.pid);
        self.by_pid.insert(child.pid, child);
    }

    pub fn lookup_by_pid(&self, pid: Pid) -> Option<&ChildState> { self.by_pid.get(&pid) }
    pub fn lookup_by_cookie(&self, cookie: u64) -> Option<&ChildState> {
        self.by_cookie.get(&cookie).and_then(|p| self.by_pid.get(p))
    }
    pub fn remove(&mut self, pid: Pid) -> Result<ChildState, ChildTableError> {
        let child = self.by_pid.remove(&pid).ok_or(ChildTableError::NotFound)?;
        self.by_cookie.remove(&child.cookie);
        Ok(child)
    }
    pub fn iter(&self) -> impl Iterator<Item = &ChildState> { self.by_pid.values() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_pid_encodes_session() {
        let mut t = ChildTable::new(5);
        let pid = t.alloc_pid().unwrap();
        assert_eq!(pid, 0x2800001);
        let pid2 = t.alloc_pid().unwrap();
        assert_eq!(pid2, 0x2800002);
    }

    #[test]
    fn insert_and_lookup() {
        let mut t = ChildTable::new(5);
        let pid = t.alloc_pid().unwrap();
        t.insert(ChildState {
            pid, local: 1, thread_tok: 0x100, cookie: 0xC0DE,
            argv0: "ls".into(), start_ticks: 0, minted_caps: vec![], pgid: None,
        });
        assert_eq!(t.lookup_by_pid(pid).unwrap().cookie, 0xC0DE);
        assert_eq!(t.lookup_by_cookie(0xC0DE).unwrap().pid, pid);
    }

    #[test]
    fn exhaustion() {
        let mut t = ChildTable::new(5);
        t.next_local = LOCAL_MAX;
        t.alloc_pid().unwrap();
        assert_eq!(t.alloc_pid(), Err(ChildTableError::Exhausted));
    }

    #[test]
    fn remove_unknown() {
        let mut t = ChildTable::new(5);
        assert_eq!(t.remove(0x2800001), Err(ChildTableError::NotFound));
    }
}
```

- [ ] **Step 2: dispatch.rs skeleton**

```rust
use procmgr_common::handler::{HandlerError, InboundMsg, Reply};

pub struct SessionState {
    pub sid: procmgr_common::pid::SessionId,
    pub generation: u32,
    pub child_table: crate::child_table::ChildTable,
    pub kernel: procmgr_common::test_kernel::MockKernel,
    // caps held by this session-procmgr:
    pub vfs_cap: u64,
    pub registry_cap: u64,
    pub timeserver_cap: u64,
}

impl SessionState {
    pub fn new_for_test(sid: u8) -> Self {
        Self {
            sid, generation: 0,
            child_table: crate::child_table::ChildTable::new(sid),
            kernel: procmgr_common::test_kernel::MockKernel::new(),
            vfs_cap: 0xV000, registry_cap: 0xR000, timeserver_cap: 0xT000,
        }
    }
}

pub fn dispatch(state: &mut SessionState, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
    match msg.label {
        // Registered in subsequent tasks.
        _ => Err(HandlerError::BadLabel),
    }
}
```

Wire `child_table` and `dispatch` into `lib.rs`:

```rust
pub mod child_table;
pub mod dispatch;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p cluu-session-procmgr --features host-test child_table
```

Expected: PASS (4 tests).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(session-procmgr): ChildTable + dispatch skeleton"
```

### Task 5.2: `Spawn` handler in session-procmgr

**Files:**
- Create: `userspace/session-procmgr/src/spawn.rs`

- [ ] **Step 1: Failing tests for every branch**

Branches to cover:
1. `success_path` — happy path: sub-mints child caps from session-held caps, allocs pid, inserts into child_table, replies pid+cookie.
2. `bad_payload_returns_badpayload`
3. `pid_exhausted_returns_eagain`
4. `cap_mint_partial_failure_rolls_back` — first sub_mint succeeds, second fails → MintGuard revokes.

```rust
extern crate alloc;
use alloc::vec::Vec;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::SESSION_PROCMGR_SPAWN_LABEL;
use procmgr_common::mint_guard::MintGuard;
use procmgr_common::wire::{SpawnReply, SpawnReq};
use crate::child_table::ChildState;
use crate::dispatch::SessionState;

pub struct Spawn;

// Child rights derived from session rights.
pub const CHILD_VFS_RIGHTS: u32 = 0x03; // read+open (write reserved)
pub const CHILD_REGISTRY_RIGHTS: u32 = 0x01;
pub const CHILD_TIMESERVER_RIGHTS: u32 = 0x01;

impl MsgHandler for Spawn {
    const LABEL: u32 = SESSION_PROCMGR_SPAWN_LABEL;
    type State = SessionState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let req: SpawnReq = postcard::from_bytes(msg.payload)
            .map_err(|_| HandlerError::BadPayload)?;
        let pid = state.child_table.alloc_pid()
            .map_err(|_| HandlerError::Eagain)?;

        let mut guard = MintGuard::new(&mut state.kernel);
        // Phase 4 invariant: child rights ⊆ session rights ⊆ root rights.
        let child_vfs = crate::cap_broker_session::sub_mint(
            &mut guard, state.vfs_cap, /*parent rights*/ 0x07, CHILD_VFS_RIGHTS)
            .map_err(|_| HandlerError::Internal("vfs"))?;
        let child_reg = crate::cap_broker_session::sub_mint(
            &mut guard, state.registry_cap, 0x03, CHILD_REGISTRY_RIGHTS)
            .map_err(|_| HandlerError::Internal("registry"))?;
        let child_ts = crate::cap_broker_session::sub_mint(
            &mut guard, state.timeserver_cap, 0x01, CHILD_TIMESERVER_RIGHTS)
            .map_err(|_| HandlerError::Internal("timeserver"))?;

        // Production: load ELF, set up address space, spawn thread.
        // For unit test: use mock kernel.
        let thread_tok = state.kernel.spawn_thread(0xE000_0000, 0xF000_0000);

        let minted = guard.forget();
        let cookie = (pid as u64) ^ 0xC0DE_0000;

        state.child_table.insert(ChildState {
            pid, local: ((pid as u32) & procmgr_common::pid::LOCAL_MAX),
            thread_tok, cookie,
            argv0: req.argv.first().cloned().unwrap_or_default(),
            start_ticks: 0,
            minted_caps: minted,
            pgid: None,
        });

        let reply = SpawnReply { pid, cookie };
        let bytes = postcard::to_allocvec(&reply).map_err(|_| HandlerError::Internal("postcard"))?;
        Ok(Reply::ok(Self::LABEL).with_payload(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::test_kernel::KernelCall;
    use procmgr_common::wire::{FdInheritEntry, FdKind};

    fn spawn_req() -> SpawnReq {
        SpawnReq {
            image_path: "/bin/ls".into(),
            argv: vec!["ls".into(), "-l".into()],
            envp: vec![],
            cwd: "/".into(),
            fd_inherit: vec![FdInheritEntry { fd: 0, kind: FdKind::Pts, cap_token: 1 }],
        }
    }

    #[test]
    fn success_path_returns_pid_cookie() {
        let mut s = SessionState::new_for_test(5);
        let payload = postcard::to_allocvec(&spawn_req()).unwrap();
        let msg = InboundMsg { label: Spawn::LABEL, words: [0; 6], payload: &payload, sender_tid: 1 };
        let reply = Spawn::handle(&mut s, &msg).unwrap();
        let r: SpawnReply = postcard::from_bytes(&reply.payload).unwrap();
        assert_eq!(r.pid, 0x2800001); // sid=5, local=1
        assert!(s.child_table.lookup_by_pid(r.pid).is_some());
    }

    #[test]
    fn bad_payload_returns_badpayload() {
        let mut s = SessionState::new_for_test(5);
        let msg = InboundMsg { label: Spawn::LABEL, words: [0; 6], payload: &[0xFF, 0xFF], sender_tid: 1 };
        assert!(matches!(Spawn::handle(&mut s, &msg), Err(HandlerError::BadPayload)));
    }

    #[test]
    fn pid_exhausted_returns_eagain() {
        let mut s = SessionState::new_for_test(5);
        s.child_table.next_local = procmgr_common::pid::LOCAL_MAX;
        s.child_table.alloc_pid().unwrap();
        let payload = postcard::to_allocvec(&spawn_req()).unwrap();
        let msg = InboundMsg { label: Spawn::LABEL, words: [0; 6], payload: &payload, sender_tid: 1 };
        assert!(matches!(Spawn::handle(&mut s, &msg), Err(HandlerError::Eagain)));
    }

    #[test]
    fn sub_mint_records_child_caps() {
        let mut s = SessionState::new_for_test(5);
        let payload = postcard::to_allocvec(&spawn_req()).unwrap();
        let msg = InboundMsg { label: Spawn::LABEL, words: [0; 6], payload: &payload, sender_tid: 1 };
        Spawn::handle(&mut s, &msg).unwrap();
        let mints: Vec<_> = s.kernel.calls.iter().filter(|c| matches!(c, KernelCall::Mint { .. })).collect();
        assert_eq!(mints.len(), 3, "vfs + registry + timeserver child caps minted");
    }

    #[test]
    fn no_orphan_caps_on_thread_spawn_failure() {
        // Simulated by an injection point: force kernel.spawn_thread to panic-like fail
        // (use a `MockKernel` variant that returns a sentinel + flag).
        //
        // Acceptance: after handler returns Err, kernel.revoke calls cover every minted cap
        // (MintGuard drops un-forgotten state).
        //
        // Implementation: extend `MockKernel` with a `fail_next_spawn: bool` field.
        // Set before call; assert revokes == mints.
        let mut s = SessionState::new_for_test(5);
        s.kernel.fail_next_spawn = true;
        let payload = postcard::to_allocvec(&spawn_req()).unwrap();
        let msg = InboundMsg { label: Spawn::LABEL, words: [0; 6], payload: &payload, sender_tid: 1 };
        let _ = Spawn::handle(&mut s, &msg);
        let mints = s.kernel.calls.iter().filter(|c| matches!(c, KernelCall::Mint { .. })).count();
        let revs = s.kernel.calls.iter().filter(|c| matches!(c, KernelCall::Revoke { .. })).count();
        assert_eq!(mints, revs, "every minted cap revoked on partial failure");
    }
}
```

(Implementing `fail_next_spawn` requires extending `MockKernel`:

```rust
// In procmgr-common/src/test_kernel.rs
pub struct MockKernel {
    pub calls: Vec<KernelCall>,
    pub next_handle: u64,
    pub revoked: BTreeMap<u64, bool>,
    pub fail_next_spawn: bool,
}
impl Kernel for MockKernel {
    fn spawn_thread(&mut self, entry: u64, stack: u64) -> u64 {
        if self.fail_next_spawn {
            self.fail_next_spawn = false;
            // Sentinel "failed" handle. Caller checks via separate API.
            return 0;
        }
        // ...existing...
    }
}
```

Update `Spawn::handle` to check `if thread_tok == 0 { return Err(HandlerError::Internal("spawn_thread")); }` — and rely on `MintGuard` drop to revoke.)

Also create a thin `cap_broker_session` wrapper in `session-procmgr/src/cap_broker_session.rs` re-using `procmgr_common::mint_guard::MintGuard` with same `sub_mint` shape as in root.

- [ ] **Step 2: Run all spawn tests**

```bash
cargo test -p cluu-session-procmgr --features host-test spawn
```

Expected: PASS (5 tests including rollback).

- [ ] **Step 3: Register Spawn in dispatch.rs match arm.**

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(session-procmgr): Spawn handler with sub-mint + MintGuard rollback"
```

---

## Phase 6 — `session-procmgr`: `child_monitor` (exit + fault)

### Task 6.1: Exit notification handler

**Files:**
- Create: `userspace/session-procmgr/src/child_monitor.rs`

- [ ] **Step 1: Failing tests for branches**

Branches:
1. Known cookie → remove from child_table, revoke child caps.
2. Unknown cookie → log + drop, no panic.
3. Restart policy = OnFailure + non-zero exit → mark for restart (state machine; actual respawn in Task 6.2).

```rust
extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_EXIT_LABEL;
use procmgr_common::wire::ExitNotif;
use procmgr_common::test_kernel::Kernel;
use crate::dispatch::SessionState;

pub struct ChildExit;

impl MsgHandler for ChildExit {
    const LABEL: u32 = PROCMGR_EXIT_LABEL;
    type State = SessionState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let cookie = msg.words[0] as u64;
        let _exit_code = msg.words[1] as i32;
        let pid = match state.child_table.lookup_by_cookie(cookie) {
            Some(c) => c.pid,
            None => return Ok(Reply::ok(Self::LABEL)), // drop unknown
        };
        let child = state.child_table.remove(pid).unwrap();
        for h in child.minted_caps {
            state.kernel.revoke(h);
        }
        Ok(Reply::ok(Self::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::test_kernel::KernelCall;
    use crate::child_table::ChildState;

    #[test]
    fn known_cookie_removes_and_revokes() {
        let mut s = SessionState::new_for_test(5);
        let pid = s.child_table.alloc_pid().unwrap();
        s.child_table.insert(ChildState {
            pid, local: 1, thread_tok: 0x100, cookie: 0xC0DE,
            argv0: "ls".into(), start_ticks: 0,
            minted_caps: vec![0xA, 0xB], pgid: None,
        });
        let msg = InboundMsg {
            label: ChildExit::LABEL,
            words: [0xC0DE, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        ChildExit::handle(&mut s, &msg).unwrap();
        assert!(s.child_table.lookup_by_pid(pid).is_none());
        let revokes: Vec<u64> = s.kernel.calls.iter().filter_map(|c| match c {
            KernelCall::Revoke { handle } => Some(*handle), _ => None,
        }).collect();
        assert_eq!(revokes, vec![0xA, 0xB]);
    }

    #[test]
    fn unknown_cookie_drops_silently() {
        let mut s = SessionState::new_for_test(5);
        let msg = InboundMsg {
            label: ChildExit::LABEL,
            words: [0xDEAD, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        ChildExit::handle(&mut s, &msg).unwrap();
        assert_eq!(s.kernel.calls.len(), 0);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p cluu-session-procmgr --features host-test child_monitor
```

Expected: PASS.

- [ ] **Step 3: Register in dispatch.**

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(session-procmgr): ChildExit handler revokes caps on child death"
```

### Task 6.2: Restart policy state machine

**Files:**
- Create: `userspace/session-procmgr/src/restart.rs`

- [ ] **Step 1: Failing tests for crash-loop threshold**

```rust
//! Per-child restart policy. Threshold: 5 restarts within 30s → mark Never,
//! log fatal. Matches spec §4.6.

extern crate alloc;
use alloc::collections::BTreeMap;

const WINDOW_TICKS: u64 = 30 * 1_000_000; // 30s in microseconds (placeholder; sync with timeserver)
const THRESHOLD: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy { Never, Always, OnFailure }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision { NoRestart, Restart, GiveUp }

#[derive(Default)]
pub struct RestartTracker {
    table: BTreeMap<u64 /* cookie */, Entry>,
}

#[derive(Debug, Clone, Copy)]
struct Entry { attempts: u32, first_attempt: u64, policy: Policy }

impl RestartTracker {
    pub fn new() -> Self { Self { table: BTreeMap::new() } }

    pub fn register(&mut self, cookie: u64, policy: Policy) {
        self.table.insert(cookie, Entry { attempts: 0, first_attempt: 0, policy });
    }

    pub fn on_exit(&mut self, cookie: u64, exit_code: i32, now: u64) -> Decision {
        let e = match self.table.get_mut(&cookie) {
            Some(e) => e,
            None => return Decision::NoRestart,
        };
        let want_restart = match e.policy {
            Policy::Never => false,
            Policy::Always => true,
            Policy::OnFailure => exit_code != 0,
        };
        if !want_restart { return Decision::NoRestart; }
        if e.attempts == 0 { e.first_attempt = now; }
        e.attempts += 1;
        if now - e.first_attempt > WINDOW_TICKS { e.attempts = 1; e.first_attempt = now; }
        if e.attempts > THRESHOLD {
            e.policy = Policy::Never;
            return Decision::GiveUp;
        }
        Decision::Restart
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_policy_no_restart() {
        let mut t = RestartTracker::new();
        t.register(1, Policy::Never);
        assert_eq!(t.on_exit(1, 1, 0), Decision::NoRestart);
    }

    #[test]
    fn always_policy_restarts_until_threshold() {
        let mut t = RestartTracker::new();
        t.register(1, Policy::Always);
        for i in 0..THRESHOLD {
            assert_eq!(t.on_exit(1, 0, i as u64), Decision::Restart);
        }
        // 6th attempt within window → give up
        assert_eq!(t.on_exit(1, 0, (THRESHOLD as u64) + 1), Decision::GiveUp);
    }

    #[test]
    fn on_failure_only_on_nonzero() {
        let mut t = RestartTracker::new();
        t.register(1, Policy::OnFailure);
        assert_eq!(t.on_exit(1, 0, 0), Decision::NoRestart);
        assert_eq!(t.on_exit(1, 1, 1), Decision::Restart);
    }

    #[test]
    fn window_reset() {
        let mut t = RestartTracker::new();
        t.register(1, Policy::Always);
        for i in 0..THRESHOLD {
            assert_eq!(t.on_exit(1, 0, i as u64), Decision::Restart);
        }
        // Far in the future → window resets
        assert_eq!(t.on_exit(1, 0, WINDOW_TICKS + 1000), Decision::Restart);
    }

    #[test]
    fn unknown_cookie_no_restart() {
        let mut t = RestartTracker::new();
        assert_eq!(t.on_exit(99, 1, 0), Decision::NoRestart);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p cluu-session-procmgr --features host-test restart
```

Expected: PASS (5 tests).

- [ ] **Step 3: Wire RestartTracker into ChildExit handler**

Extend `ChildExit::handle` to consult `state.restart` and either respawn (via spawn logic — extract to helper) or drop.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(session-procmgr): RestartTracker with crash-loop threshold"
```

---

## Phase 7 — `session-procmgr`: `kill`, `pg_table`, `ctty`

### Task 7.1: Port `pg_table.rs` to session-procmgr

**Files:**
- Move: `userspace/root-procmgr/src/pg_table.rs` → `userspace/session-procmgr/src/pg_table.rs`

- [ ] **Step 1: Move file**

```bash
git mv userspace/root-procmgr/src/pg_table.rs userspace/session-procmgr/src/pg_table.rs
```

- [ ] **Step 2: Add unit tests** (file currently has no tests):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_attach_detach() {
        let mut t = PgTable::new();
        let p = t.create();
        t.attach(p, 100);
        t.attach(p, 100); // idempotent
        assert_eq!(t.members(p), vec![100]);
        t.detach(p, 100);
        assert!(!t.exists(p), "empty group dropped");
    }

    #[test]
    fn pgid_of_finds_member() {
        let mut t = PgTable::new();
        let p1 = t.create();
        let p2 = t.create();
        t.attach(p1, 10);
        t.attach(p2, 20);
        assert_eq!(t.pgid_of(10), Some(p1));
        assert_eq!(t.pgid_of(20), Some(p2));
        assert_eq!(t.pgid_of(99), None);
    }

    #[test]
    fn detach_unknown_pid_idempotent() {
        let mut t = PgTable::new();
        let p = t.create();
        t.detach(p, 99); // no panic
        assert!(t.exists(p));
    }
}
```

- [ ] **Step 3: Add handler wrappers**

Create `userspace/session-procmgr/src/pg_handlers.rs` with `PgCreate`, `PgAttach`, `PgSignal` MsgHandler impls (analogous to spawn). Tests per branch.

- [ ] **Step 4: Run tests + register handlers**

```bash
cargo test -p cluu-session-procmgr --features host-test pg
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(session-procmgr): port pg_table + add pg handler tests"
```

### Task 7.2: `Kill` handler

**Files:**
- Create: `userspace/session-procmgr/src/kill.rs`

- [ ] **Step 1: Failing tests for branches**

```rust
extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::SESSION_PROCMGR_KILL_LABEL;
use procmgr_common::pid::Pid;
use procmgr_common::test_kernel::Kernel;
use crate::dispatch::SessionState;

pub struct Kill;

impl MsgHandler for Kill {
    const LABEL: u32 = SESSION_PROCMGR_KILL_LABEL;
    type State = SessionState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let pid = msg.words[0] as Pid;
        let signal = msg.words[1] as u32;
        let child = state.child_table.lookup_by_pid(pid).ok_or(HandlerError::NotFound)?;
        // SIGKILL = thread terminate; SIGTERM = signal; others later.
        // For now use kernel.revoke on thread_tok as terminate.
        if signal == 9 /* SIGKILL */ {
            state.kernel.revoke(child.thread_tok);
        }
        Ok(Reply::ok(Self::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::test_kernel::KernelCall;
    use crate::child_table::ChildState;

    #[test]
    fn kill_sigkill_revokes_thread() {
        let mut s = SessionState::new_for_test(5);
        let pid = s.child_table.alloc_pid().unwrap();
        s.child_table.insert(ChildState {
            pid, local: 1, thread_tok: 0xT001, cookie: 0xC0DE,
            argv0: "ls".into(), start_ticks: 0, minted_caps: vec![], pgid: None,
        });
        let msg = InboundMsg {
            label: Kill::LABEL, words: [pid as usize, 9, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        Kill::handle(&mut s, &msg).unwrap();
        assert!(s.kernel.calls.iter().any(|c| matches!(c, KernelCall::Revoke { handle } if *handle == 0xT001)));
    }

    #[test]
    fn kill_unknown_pid_returns_notfound() {
        let mut s = SessionState::new_for_test(5);
        let msg = InboundMsg {
            label: Kill::LABEL, words: [0x2800999, 9, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        assert!(matches!(Kill::handle(&mut s, &msg), Err(HandlerError::NotFound)));
    }

    #[test]
    fn kill_pid_from_other_session_returns_notfound() {
        let mut s = SessionState::new_for_test(5);
        // pid encoded for session 7 — this session-procmgr never sees it.
        let pid: Pid = (7i32 << 23) | 1;
        let msg = InboundMsg {
            label: Kill::LABEL, words: [pid as usize, 9, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        assert!(matches!(Kill::handle(&mut s, &msg), Err(HandlerError::NotFound)));
    }
}
```

- [ ] **Step 2: Run tests + register in dispatch**

```bash
cargo test -p cluu-session-procmgr --features host-test kill
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(session-procmgr): Kill handler — sigkill via thread revoke"
```

### Task 7.3: `Ctty` query handler

**Files:**
- Create: `userspace/session-procmgr/src/ctty.rs`

- [ ] **Step 1: Failing tests**

```rust
extern crate alloc;
use alloc::string::String;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::SESSION_PROCMGR_CTTY_QUERY_LABEL;
use crate::dispatch::SessionState;

pub struct CttyQuery;

impl MsgHandler for CttyQuery {
    const LABEL: u32 = SESSION_PROCMGR_CTTY_QUERY_LABEL;
    type State = SessionState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let ctty_path = state.ctty.clone().ok_or(HandlerError::NotFound)?;
        let bytes = postcard::to_allocvec(&ctty_path).map_err(|_| HandlerError::Internal("postcard"))?;
        Ok(Reply::ok(Self::LABEL).with_payload(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_ctty_returns_notfound() {
        let mut s = SessionState::new_for_test(5);
        let msg = InboundMsg { label: CttyQuery::LABEL, words: [0; 6], payload: &[], sender_tid: 1 };
        assert!(matches!(CttyQuery::handle(&mut s, &msg), Err(HandlerError::NotFound)));
    }

    #[test]
    fn ctty_set_returns_path() {
        let mut s = SessionState::new_for_test(5);
        s.ctty = Some("/dev/pts/5".into());
        let msg = InboundMsg { label: CttyQuery::LABEL, words: [0; 6], payload: &[], sender_tid: 1 };
        let r = CttyQuery::handle(&mut s, &msg).unwrap();
        let path: String = postcard::from_bytes(&r.payload).unwrap();
        assert_eq!(path, "/dev/pts/5");
    }
}
```

Add `pub ctty: Option<String>` to `SessionState`.

- [ ] **Step 2: Run tests + register**

```bash
cargo test -p cluu-session-procmgr --features host-test ctty
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(session-procmgr): CttyQuery handler"
```

---

## Phase 8 — `session-procmgr`: `pipe_registry`

### Task 8.1: PipeRegistry data structure

**Files:**
- Create: `userspace/session-procmgr/src/pipe_registry.rs`

- [ ] **Step 1: Failing tests for create/close/lookup**

```rust
extern crate alloc;
use alloc::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Pipe {
    pub id: u64,
    pub read_cap: u64,
    pub write_cap: u64,
    pub buffer_cap: u64,
}

pub struct PipeRegistry {
    next_id: u64,
    pipes: BTreeMap<u64, Pipe>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PipeError { NotFound }

impl PipeRegistry {
    pub fn new() -> Self { Self { next_id: 1, pipes: BTreeMap::new() } }
    pub fn create(&mut self, read_cap: u64, write_cap: u64, buffer_cap: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.pipes.insert(id, Pipe { id, read_cap, write_cap, buffer_cap });
        id
    }
    pub fn lookup(&self, id: u64) -> Option<&Pipe> { self.pipes.get(&id) }
    pub fn close(&mut self, id: u64) -> Result<Pipe, PipeError> {
        self.pipes.remove(&id).ok_or(PipeError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_returns_distinct_ids() {
        let mut r = PipeRegistry::new();
        let a = r.create(0xA0, 0xA1, 0xA2);
        let b = r.create(0xB0, 0xB1, 0xB2);
        assert_ne!(a, b);
    }

    #[test]
    fn close_known() {
        let mut r = PipeRegistry::new();
        let id = r.create(1, 2, 3);
        let p = r.close(id).unwrap();
        assert_eq!(p.read_cap, 1);
    }

    #[test]
    fn close_unknown() {
        let mut r = PipeRegistry::new();
        assert_eq!(r.close(999), Err(PipeError::NotFound));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p cluu-session-procmgr --features host-test pipe_registry
```

Expected: PASS.

- [ ] **Step 3: Add `PipeCreate` / `PipeClose` handlers** in `pipe_handlers.rs`. Tests per branch.

- [ ] **Step 4: Run + register + commit**

```bash
git add -A
git commit -m "feat(session-procmgr): pipe_registry + PipeCreate/Close handlers"
```

---

## Phase 9 — `root-procmgr`: `proc_query_all` (SYSTEM-cap-gated aggregator)

### Task 9.1: `proc_query_local` in session-procmgr

**Files:**
- Create: `userspace/session-procmgr/src/proc_query_local.rs`

- [ ] **Step 1: Failing tests**

```rust
extern crate alloc;
use alloc::vec::Vec;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::SESSION_PROCMGR_PROC_QUERY_LOCAL_LABEL;
use procmgr_common::wire::{ProcInfo, ProcQueryLocalReply, ProcQueryLocalReq};
use crate::dispatch::SessionState;

pub struct ProcQueryLocal;

impl MsgHandler for ProcQueryLocal {
    const LABEL: u32 = SESSION_PROCMGR_PROC_QUERY_LOCAL_LABEL;
    type State = SessionState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let req: ProcQueryLocalReq = postcard::from_bytes(msg.payload)
            .map_err(|_| HandlerError::BadPayload)?;
        let mut procs = Vec::new();
        for c in state.child_table.iter() {
            if req.pids.is_empty() || req.pids.contains(&c.pid) {
                procs.push(ProcInfo {
                    pid: c.pid,
                    ppid: 0, // session-procmgr is parent; root-procmgr decorates with itself if needed
                    state: 1,
                    command: c.argv0.clone(),
                    argv0: c.argv0.clone(),
                    start_ticks: c.start_ticks,
                });
            }
        }
        let reply = ProcQueryLocalReply { procs };
        let bytes = postcard::to_allocvec(&reply).map_err(|_| HandlerError::Internal("postcard"))?;
        Ok(Reply::ok(Self::LABEL).with_payload(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::child_table::ChildState;

    #[test]
    fn empty_session_returns_empty() {
        let mut s = SessionState::new_for_test(5);
        let payload = postcard::to_allocvec(&ProcQueryLocalReq { pids: vec![] }).unwrap();
        let msg = InboundMsg { label: ProcQueryLocal::LABEL, words: [0; 6], payload: &payload, sender_tid: 1 };
        let r = ProcQueryLocal::handle(&mut s, &msg).unwrap();
        let reply: ProcQueryLocalReply = postcard::from_bytes(&r.payload).unwrap();
        assert!(reply.procs.is_empty());
    }

    #[test]
    fn returns_all_children() {
        let mut s = SessionState::new_for_test(5);
        for i in 0..3 {
            let pid = s.child_table.alloc_pid().unwrap();
            s.child_table.insert(ChildState {
                pid, local: i + 1, thread_tok: 0, cookie: i as u64,
                argv0: alloc::format!("p{}", i),
                start_ticks: 0, minted_caps: vec![], pgid: None,
            });
        }
        let payload = postcard::to_allocvec(&ProcQueryLocalReq { pids: vec![] }).unwrap();
        let msg = InboundMsg { label: ProcQueryLocal::LABEL, words: [0; 6], payload: &payload, sender_tid: 1 };
        let r = ProcQueryLocal::handle(&mut s, &msg).unwrap();
        let reply: ProcQueryLocalReply = postcard::from_bytes(&r.payload).unwrap();
        assert_eq!(reply.procs.len(), 3);
    }

    #[test]
    fn filter_specific_pids() {
        let mut s = SessionState::new_for_test(5);
        let p1 = s.child_table.alloc_pid().unwrap();
        let p2 = s.child_table.alloc_pid().unwrap();
        for (pid, name) in [(p1, "a"), (p2, "b")] {
            s.child_table.insert(ChildState {
                pid, local: (pid as u32) & 0x7FFFFF, thread_tok: 0, cookie: pid as u64,
                argv0: name.into(), start_ticks: 0, minted_caps: vec![], pgid: None,
            });
        }
        let payload = postcard::to_allocvec(&ProcQueryLocalReq { pids: vec![p1] }).unwrap();
        let msg = InboundMsg { label: ProcQueryLocal::LABEL, words: [0; 6], payload: &payload, sender_tid: 1 };
        let r = ProcQueryLocal::handle(&mut s, &msg).unwrap();
        let reply: ProcQueryLocalReply = postcard::from_bytes(&r.payload).unwrap();
        assert_eq!(reply.procs.len(), 1);
        assert_eq!(reply.procs[0].argv0, "a");
    }
}
```

- [ ] **Step 2: Run + register + commit**

```bash
cargo test -p cluu-session-procmgr --features host-test proc_query_local
git add -A && git commit -m "feat(session-procmgr): ProcQueryLocal handler"
```

### Task 9.2: `proc_query_all` in root-procmgr

**Files:**
- Create: `userspace/root-procmgr/src/proc_query_all.rs`

- [ ] **Step 1: Failing tests with SYSTEM-cap gate**

```rust
extern crate alloc;
use alloc::vec::Vec;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_PROC_QUERY_ALL_LABEL;
use procmgr_common::wire::{ProcInfo, ProcQueryLocalReply};
use crate::dispatch::ProcmgrState;

pub struct ProcQueryAll;

pub const SYSTEM_PROC_QUERY_CAP_ID: u64 = 0xCAF_E000_0000_0001;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProcQueryAllReply {
    pub procs: Vec<(u8 /* sid */, ProcInfo)>,
}

impl MsgHandler for ProcQueryAll {
    const LABEL: u32 = PROCMGR_PROC_QUERY_ALL_LABEL;
    type State = ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        // SYSTEM cap presented via words[0] (cap_id).
        let presented = msg.words[0] as u64;
        if presented != SYSTEM_PROC_QUERY_CAP_ID {
            return Err(HandlerError::BadCap);
        }
        // Aggregate by querying every session-procmgr.
        // Stub: tests inject a mock query function via `state.query_session_local`.
        let mut all: Vec<(u8, ProcInfo)> = Vec::new();
        let snapshot: Vec<_> = state.session_directory.iter().map(|e| (e.sid, e.session_pmgr_spawn_ep)).collect();
        for (sid, _ep) in snapshot {
            let reply = (state.query_session_local)(sid);
            for p in reply.procs {
                all.push((sid, p));
            }
        }
        let bytes = postcard::to_allocvec(&ProcQueryAllReply { procs: all })
            .map_err(|_| HandlerError::Internal("postcard"))?;
        Ok(Reply::ok(Self::LABEL).with_payload(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_local(_sid: u8) -> ProcQueryLocalReply {
        ProcQueryLocalReply { procs: vec![] }
    }

    #[test]
    fn missing_cap_returns_badcap() {
        let mut s = ProcmgrState::new_for_test();
        s.query_session_local = empty_local;
        let msg = InboundMsg { label: ProcQueryAll::LABEL, words: [0, 0, 0, 0, 0, 0], payload: &[], sender_tid: 1 };
        assert!(matches!(ProcQueryAll::handle(&mut s, &msg), Err(HandlerError::BadCap)));
    }

    #[test]
    fn cap_present_returns_aggregate() {
        let mut s = ProcmgrState::new_for_test();
        // seed two sessions
        for _ in 0..2 {
            let (sid, gen) = s.session_directory.alloc_sid().unwrap();
            s.session_directory.insert(crate::session_directory::SessionEntry {
                sid, generation: gen, user_name: "u".into(),
                session_pmgr_thread_tok: 0, session_pmgr_spawn_ep: 0,
                minted_caps: vec![], state: crate::session_directory::SessionState::Live,
            });
        }
        s.query_session_local = |sid| ProcQueryLocalReply {
            procs: vec![ProcInfo {
                pid: ((sid as i32) << 23) | 1, ppid: 0, state: 1,
                command: "x".into(), argv0: "x".into(), start_ticks: 0,
            }],
        };
        let msg = InboundMsg {
            label: ProcQueryAll::LABEL,
            words: [SYSTEM_PROC_QUERY_CAP_ID as usize, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        let r = ProcQueryAll::handle(&mut s, &msg).unwrap();
        let reply: ProcQueryAllReply = postcard::from_bytes(&r.payload).unwrap();
        assert_eq!(reply.procs.len(), 2);
    }
}
```

Add fn pointer field to `ProcmgrState`:

```rust
pub query_session_local: fn(u8) -> procmgr_common::wire::ProcQueryLocalReply,
```

(Production: real IPC call. Tests: lambda.)

- [ ] **Step 2: Run + register + commit**

```bash
cargo test -p cluu-root-procmgr --features host-test proc_query_all
git add -A && git commit -m "feat(root-procmgr): ProcQueryAll handler with SYSTEM-cap gate"
```

---

## Phase 10 — `root-procmgr`: services + restart

### Task 10.1: Service spawn (vfs/registry/timeserver/virtio-blk)

**Files:**
- Create: `userspace/root-procmgr/src/services.rs`

- [ ] **Step 1: Failing tests**

```rust
extern crate alloc;
use alloc::string::String;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_SERVICE_SPAWN_LABEL;

pub struct ServiceSpawn;

#[derive(serde::Deserialize)]
pub struct ServiceSpawnReq { pub name: String, pub image_path: String }

#[derive(Debug, Clone)]
pub struct ServiceEntry {
    pub name: String,
    pub thread_tok: u64,
    pub publish_cap: u64,
    pub restart_policy: crate::restart_root::Policy,
}

impl MsgHandler for ServiceSpawn {
    const LABEL: u32 = PROCMGR_SERVICE_SPAWN_LABEL;
    type State = crate::dispatch::ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let req: ServiceSpawnReq = postcard::from_bytes(msg.payload)
            .map_err(|_| HandlerError::BadPayload)?;
        // Real: load ELF, spawn thread, publish service cap via registry.
        // Mock: kernel.spawn_thread + record.
        let thread_tok = state.kernel.spawn_thread(0xE000_0000, 0xF000_0000);
        let publish_cap = state.kernel.mint(0xPUBLISH, 0xFF);
        state.services.push(ServiceEntry {
            name: req.name, thread_tok, publish_cap,
            restart_policy: crate::restart_root::Policy::Always,
        });
        Ok(Reply::ok(Self::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::test_kernel::KernelCall;

    #[test]
    fn spawn_vfs_records_thread_and_publish_cap() {
        let mut s = crate::dispatch::ProcmgrState::new_for_test();
        let req = ServiceSpawnReq { name: "vfs".into(), image_path: "/sbin/vfs".into() };
        let p = postcard::to_allocvec(&req).unwrap();
        let msg = InboundMsg { label: ServiceSpawn::LABEL, words: [0; 6], payload: &p, sender_tid: 1 };
        ServiceSpawn::handle(&mut s, &msg).unwrap();
        assert_eq!(s.services.len(), 1);
        assert_eq!(s.services[0].name, "vfs");
        let spawns: usize = s.kernel.calls.iter().filter(|c| matches!(c, KernelCall::SpawnThread { .. })).count();
        let mints: usize = s.kernel.calls.iter().filter(|c| matches!(c, KernelCall::Mint { .. })).count();
        assert_eq!(spawns, 1);
        assert_eq!(mints, 1);
    }

    #[test]
    fn bad_payload() {
        let mut s = crate::dispatch::ProcmgrState::new_for_test();
        let msg = InboundMsg { label: ServiceSpawn::LABEL, words: [0; 6], payload: &[0xFF], sender_tid: 1 };
        assert!(matches!(ServiceSpawn::handle(&mut s, &msg), Err(HandlerError::BadPayload)));
    }
}
```

Add `pub services: Vec<services::ServiceEntry>` to `ProcmgrState`.

- [ ] **Step 2: Run + register + commit**

```bash
cargo test -p cluu-root-procmgr --features host-test services
git add -A && git commit -m "feat(root-procmgr): ServiceSpawn handler"
```

### Task 10.2: Root-side restart policy

**Files:**
- Create: `userspace/root-procmgr/src/restart_root.rs`

Duplicate (briefly) the `RestartTracker` shape from session-procmgr but tracking services. Test similarly. Commit.

```bash
git add -A && git commit -m "feat(root-procmgr): restart_root tracker for services + session-procmgrs"
```

---

## Phase 11 — `root-procmgr`: `escalate`, `shutdown`

### Task 11.1: `Escalate` handler

**Files:**
- Create: `userspace/root-procmgr/src/escalate.rs`

- [ ] **Step 1: Tests**

```rust
//! Escalation: hand a holder of an "escalate-cap" a SYSTEM cap-bundle
//! (e.g. for sudo). Strict cap model — escalate-cap is what gates,
//! no identity lookup.

extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_ESCALATE_LABEL;
use crate::dispatch::ProcmgrState;

pub struct Escalate;
pub const ESCALATE_CAP_ID: u64 = 0xCAFE_E5CA_LATE_0001u64;

impl MsgHandler for Escalate {
    const LABEL: u32 = PROCMGR_ESCALATE_LABEL;
    type State = ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        if (msg.words[0] as u64) != ESCALATE_CAP_ID { return Err(HandlerError::BadCap); }
        let granted = state.kernel.mint(0xSYSTEM_BUNDLE, 0xFFFF_FFFF);
        Ok(Reply::ok(Self::LABEL).with_word(0, granted as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cap() {
        let mut s = ProcmgrState::new_for_test();
        let msg = InboundMsg { label: Escalate::LABEL, words: [0; 6], payload: &[], sender_tid: 1 };
        assert!(matches!(Escalate::handle(&mut s, &msg), Err(HandlerError::BadCap)));
    }

    #[test]
    fn cap_present_grants_bundle() {
        let mut s = ProcmgrState::new_for_test();
        let msg = InboundMsg {
            label: Escalate::LABEL,
            words: [ESCALATE_CAP_ID as usize, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        let r = Escalate::handle(&mut s, &msg).unwrap();
        assert_ne!(r.words[0], 0);
    }
}
```

- [ ] **Step 2: Run + register + commit**

```bash
cargo test -p cluu-root-procmgr --features host-test escalate
git add -A && git commit -m "feat(root-procmgr): Escalate handler (cap-gated)"
```

### Task 11.2: `Shutdown` handler

**Files:**
- Create: `userspace/root-procmgr/src/shutdown.rs`

- [ ] **Step 1: Tests** (sequence: tear down sessions in reverse-creation order, then services, then signal init).

```rust
extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_SHUTDOWN_LABEL;
use crate::dispatch::ProcmgrState;

pub struct Shutdown;
pub const SHUTDOWN_CAP_ID: u64 = 0xCAFE_DEAD_BEEF_0001u64;

impl MsgHandler for Shutdown {
    const LABEL: u32 = PROCMGR_SHUTDOWN_LABEL;
    type State = ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        if (msg.words[0] as u64) != SHUTDOWN_CAP_ID { return Err(HandlerError::BadCap); }
        // Sessions in reverse order
        let sids: Vec<u8> = state.session_directory.iter().map(|e| e.sid).collect();
        for sid in sids.into_iter().rev() {
            let _ = state.session_directory.mark_dying(sid);
            if let Ok(caps) = state.session_directory.finalise_dead(sid) {
                for c in caps { state.kernel.revoke(c); }
            }
        }
        // Services
        for svc in state.services.drain(..) {
            state.kernel.revoke(svc.publish_cap);
            state.kernel.revoke(svc.thread_tok);
        }
        // Mark global shutdown flag — init monitors it
        state.shutting_down = true;
        Ok(Reply::ok(Self::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cap() {
        let mut s = ProcmgrState::new_for_test();
        let msg = InboundMsg { label: Shutdown::LABEL, words: [0; 6], payload: &[], sender_tid: 1 };
        assert!(matches!(Shutdown::handle(&mut s, &msg), Err(HandlerError::BadCap)));
    }

    #[test]
    fn shutdown_revokes_all() {
        let mut s = ProcmgrState::new_for_test();
        let (sid, gen) = s.session_directory.alloc_sid().unwrap();
        s.session_directory.insert(crate::session_directory::SessionEntry {
            sid, generation: gen, user_name: "u".into(),
            session_pmgr_thread_tok: 0, session_pmgr_spawn_ep: 0,
            minted_caps: vec![0xA1, 0xA2],
            state: crate::session_directory::SessionState::Live,
        });
        s.services.push(crate::services::ServiceEntry {
            name: "vfs".into(), thread_tok: 0xS1, publish_cap: 0xS2,
            restart_policy: crate::restart_root::Policy::Always,
        });
        let msg = InboundMsg {
            label: Shutdown::LABEL,
            words: [SHUTDOWN_CAP_ID as usize, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        Shutdown::handle(&mut s, &msg).unwrap();
        assert!(s.shutting_down);
        let revokes: usize = s.kernel.calls.iter().filter(|c|
            matches!(c, procmgr_common::test_kernel::KernelCall::Revoke { .. })
        ).count();
        assert_eq!(revokes, 4); // 2 session caps + svc thread + svc publish
    }
}
```

Add `pub shutting_down: bool` to `ProcmgrState`.

- [ ] **Step 2: Run + register + commit**

```bash
cargo test -p cluu-root-procmgr --features host-test shutdown
git add -A && git commit -m "feat(root-procmgr): Shutdown handler with sequenced cascade"
```

---

## Phase 12 — Bootstrap rewire (init, login)

### Task 12.1: Real kernel surface

**Files:**
- Create: `userspace/root-procmgr/src/real_kernel.rs`
- Create: `userspace/session-procmgr/src/real_kernel.rs`

- [ ] **Step 1: Production `Kernel` impl wrapping `libcluu::syscall`**

```rust
//! Production wiring of the Kernel trait to actual syscalls.

use procmgr_common::test_kernel::Kernel;

pub struct RealKernel;

impl Kernel for RealKernel {
    fn mint(&mut self, parent: u64, rights: u32) -> u64 {
        libcluu::syscall::cap_mint(parent, rights).unwrap_or(0)
    }
    fn revoke(&mut self, handle: u64) {
        let _ = libcluu::syscall::cap_revoke(handle);
    }
    fn spawn_thread(&mut self, entry: u64, stack: u64) -> u64 {
        libcluu::syscall::thread_create(entry, stack).unwrap_or(0)
    }
}
```

(If `libcluu::syscall` does not expose these by these names yet, add thin wrappers in `libcluu` first; matching names are: `cap_mint`, `cap_revoke`, `thread_create`. Grep existing libcluu for exact functions and adapt.)

- [ ] **Step 2: Update `ProcmgrState::new()` (non-test) to use `RealKernel`**

Type-erase via `Box<dyn Kernel>` or generic. Recommended: generic parameter on state.

```rust
pub struct ProcmgrState<K: Kernel = procmgr_common::test_kernel::MockKernel> {
    pub kernel: K,
    // …
}
```

(In production binaries instantiate `ProcmgrState::<RealKernel>::new()`. Tests use the default `MockKernel`.)

- [ ] **Step 3: Build**

```bash
cargo xtask build
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat: RealKernel adapters wiring procmgr-common Kernel trait to syscalls"
```

### Task 12.2: Init spawns root-procmgr (primordial)

**Files:**
- Modify: `userspace/init/src/main.rs`

- [ ] **Step 1: Replace any `procmgr` spawn calls with `root-procmgr`**

```bash
grep -n 'procmgr' userspace/init/src/main.rs
```

Update binary paths/strings. Verify init still monitors root-procmgr on `exit_endpoint`.

- [ ] **Step 2: Build + boot smoke**

```bash
cargo xtask build
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
```

Expected: boot reaches at least the init → root-procmgr → vfs handoff. (Login flow not yet rewired — failures past that line are expected.)

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "chore(init): spawn root-procmgr (renamed binary)"
```

### Task 12.3: Login → SESSION_CREATE → session-procmgr spawn

**Files:**
- Modify: `userspace/login/src/main.rs`
- Modify: `userspace/root-procmgr/src/session_directory.rs` (real session-procmgr spawn inside `SessionCreate::handle` production path)

- [ ] **Step 1: Login change — replace direct spawn-shell call with SESSION_CREATE IPC**

Locate the current login post-auth path and replace the legacy `PROCMGR_CONTAINER_RUN` IPC with a `PROCMGR_SESSION_CREATE` IPC. Receive the `SessionEnvelope`, then call the spawn-endpoint contained in the envelope to spawn shell.

- [ ] **Step 2: `SessionCreate::handle` production path**

When NOT under `#[cfg(test)]`, after minting caps:

```rust
// Spawn session-procmgr binary, hand it the envelope.
let envelope_bytes = postcard::to_allocvec(&envelope).map_err(|_| HandlerError::Internal("postcard"))?;
let (pmgr_tid, pmgr_ep) = spawn_session_procmgr(&envelope_bytes, &minted_caps)?;
```

Where `spawn_session_procmgr` is a helper performing: ELF load of `/sbin/session-procmgr`, allocate address space, copy envelope into child's FdInherit slot, register fault/exit endpoint pointing at root-procmgr, start thread.

- [ ] **Step 3: Boot smoke**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_login bash scripts/harness_run.sh
```

Expected: login → session create → shell. Verify shell prompt visible. If broken, debug iteratively before committing.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat: login uses SESSION_CREATE; root spawns session-procmgr"
```

### Task 12.4: Delete legacy spawn bypass

**Files:**
- Modify: `userspace/root-procmgr/src/main.rs` (delete `handle_spawn_unified` legacy bypass)

- [ ] **Step 1: Grep for bypass markers per `project_spawn_hooks_unwired` memory**

```bash
grep -n 'spawn_service_with_env\|bypass\|TODO.*spawn' userspace/root-procmgr/src/main.rs
```

- [ ] **Step 2: Delete bypass code path**

Remove the `handle_spawn_unified` legacy bypass and any code unreachable after the session-procmgr is the only spawn entrypoint for user procs. Service spawn (vfs etc.) goes through `ServiceSpawn` handler.

- [ ] **Step 3: Build + smoke**

```bash
cargo xtask build
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_login bash scripts/harness_run.sh
```

Expected: login still works without bypass.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor(root-procmgr): delete legacy spawn bypass"
```

---

## Phase 13 — Cap-purity lint + integration test suite

### Task 13.1: `xtask check-cap-purity`

**Files:**
- Modify: `xtask/src/main.rs`

- [ ] **Step 1: Add subcommand**

```rust
fn check_cap_purity() -> anyhow::Result<()> {
    let forbidden = [
        "pid_to_session", "tid_to_pid", "resolve_caller_session",
        "caller_profile", "can_grant", "session_match",
    ];
    let crates = ["userspace/root-procmgr", "userspace/session-procmgr"];
    let mut hits = Vec::new();
    for c in &crates {
        for f in walkdir::WalkDir::new(c).into_iter().filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |x| x == "rs"))
        {
            let body = std::fs::read_to_string(f.path())?;
            for kw in &forbidden {
                for (line, text) in body.lines().enumerate() {
                    if text.contains(kw) && !text.trim_start().starts_with("//") {
                        hits.push(format!("{}:{}: {}", f.path().display(), line + 1, kw));
                    }
                }
            }
        }
    }
    if !hits.is_empty() {
        for h in &hits { eprintln!("cap-purity violation: {}", h); }
        anyhow::bail!("{} cap-purity violations", hits.len());
    }
    Ok(())
}
```

Register as `cargo xtask check-cap-purity`.

- [ ] **Step 2: Run and ensure clean exit on current refactored tree**

```bash
cargo xtask check-cap-purity
```

Expected: PASS (no violations remain).

- [ ] **Step 3: Add CI gate doc note**

In `xtask/README.md` or root `README.md`, note that `cargo xtask check-cap-purity` is required pre-commit.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(xtask): check-cap-purity grep gate"
```

### Task 13.2: `pm_*` integration tests

For each `pm_*` marker in the spec (§5.6), create a userspace test binary under `userspace/probes/pm_<name>/`, register in workspace, write the test driver, run via harness.

Tests required:
- `pm_bootstrap_two_pmgr`
- `pm_session_crash_cascade`
- `pm_cap_revoke_stale`
- `pm_session_id_recycle`
- `pm_cross_session_no_leak`
- `pm_proc_query_all_cap`
- `pm_pid_layout`
- `pm_service_restart`

#### Task 13.2.a: `pm_pid_layout` (representative example)

**Files:**
- Create: `userspace/probes/pm_pid_layout/Cargo.toml`
- Create: `userspace/probes/pm_pid_layout/src/main.rs`

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "pm-pid-layout"
version = "0.1.0"
edition = "2021"

[dependencies]
libcluu = { path = "../../libcluu" }
cluu_wire = { path = "../../cluu_wire" }
procmgr-common = { path = "../../libs/procmgr-common" }

[[bin]]
name = "pm_pid_layout"
path = "src/main.rs"
```

- [ ] **Step 2: src/main.rs**

```rust
#![no_std]
#![no_main]
extern crate alloc;
use libcluu::println;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // Spawn 3 children in current session; ensure PIDs all share same high byte.
    let pids = [
        libcluu::process::spawn_capability("/bin/true", &[]),
        libcluu::process::spawn_capability("/bin/true", &[]),
        libcluu::process::spawn_capability("/bin/true", &[]),
    ];
    let sids: alloc::vec::Vec<u8> = pids.iter()
        .map(|p| ((*p as u32) >> 23) as u8)
        .collect();
    let same = sids.windows(2).all(|w| w[0] == w[1]);
    if same { println!("MARKER:pm_pid_layout:PASS"); 0 } else { println!("MARKER:pm_pid_layout:FAIL"); 1 }
}
```

- [ ] **Step 3: Add to workspace + Cluufile**

- [ ] **Step 4: Run**

```bash
HARNESS_FORCE_BUILD=1 CLUU_SHELL_AUTOSTART_CMD=pm_pid_layout MARKER_MODE=pm_pid_layout bash scripts/harness_run.sh
grep MARKER serial.log
```

Expected: `MARKER:pm_pid_layout:PASS`.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(pm): pm_pid_layout integration marker"
```

#### Task 13.2.b-h: Repeat structure for each remaining `pm_*` marker

Each marker follows the same template:
1. New probe crate under `userspace/probes/pm_<name>/`.
2. Implements its specific assertion (cap-revoke yields EBADTOK; sid recycle yields old-cap EBADTOK; cross-session vfs cap is opaque to other session; etc.).
3. Commit.

(Detailed code per marker: cap_revoke_stale probe holds a child cap, asks root to destroy the session, then attempts to use the cap and asserts EBADTOK; session_id_recycle probe creates+destroys+recreates session, attempts old envelope, etc. Following the same `MARKER:<name>:PASS` / `:FAIL` convention.)

---

## Phase 14 — Coverage gates + acceptance + merge

### Task 14.1: `cargo llvm-cov` integration

**Files:**
- Modify: `xtask/src/main.rs`

- [ ] **Step 1: Add `xtask coverage-check` subcommand**

```rust
fn coverage_check() -> anyhow::Result<()> {
    // Requires cargo-llvm-cov installed; CI step ensures so.
    let crates = ["procmgr-common", "cluu-root-procmgr", "cluu-session-procmgr"];
    let thresholds = [("line", 95.0), ("branch", 95.0)];
    let out = std::process::Command::new("cargo")
        .args(["llvm-cov", "--summary-only", "--features", "host-test", "--json"])
        .args(crates.iter().flat_map(|c| ["-p", c]))
        .output()?;
    if !out.status.success() {
        anyhow::bail!("cargo llvm-cov failed");
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let totals = &json["data"][0]["totals"];
    for (key, threshold) in thresholds {
        let pct = totals[key]["percent"].as_f64().unwrap_or(0.0);
        eprintln!("coverage: {} = {:.2}% (threshold {:.0}%)", key, pct, threshold);
        if pct < threshold {
            anyhow::bail!("{} coverage {:.2}% below {:.0}%", key, pct, threshold);
        }
    }
    Ok(())
}
```

Add `serde_json` to `xtask/Cargo.toml`.

- [ ] **Step 2: Run**

```bash
cargo install cargo-llvm-cov || true
cargo xtask coverage-check
```

Expected: PASS if coverage ≥ 95 % line+branch. If under, write more tests until green.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(xtask): coverage-check gate at 95% line+branch"
```

### Task 14.2: Coverage matrix doc

**Files:**
- Create: `docs/superpowers/specs/PROCMGR_CAP_REFACTOR_COVERAGE.md`

- [ ] **Step 1: Build matrix**

```markdown
# procmgr Cap-Refactor Coverage Matrix

| Handler              | Branches | Tests covering each branch                                   |
|----------------------|---------:|---------------------------------------------------------------|
| `SessionCreate`      |        3 | `create_returns_envelope_with_pid_base`, `create_bad_payload_returns_badpayload`, `create_exhausted_returns_eagain` |
| `SessionDestroy`     |        3 | `destroy_revokes_all_minted_caps`, `destroy_unknown_sid_returns_notfound`, `destroy_bumps_generation_for_sid_reuse` |
| `Spawn`              |        5 | `success_path_returns_pid_cookie`, `bad_payload_returns_badpayload`, `pid_exhausted_returns_eagain`, `sub_mint_records_child_caps`, `no_orphan_caps_on_thread_spawn_failure` |
| `Kill`               |        3 | `kill_sigkill_revokes_thread`, `kill_unknown_pid_returns_notfound`, `kill_pid_from_other_session_returns_notfound` |
| `ChildExit`          |        2 | `known_cookie_removes_and_revokes`, `unknown_cookie_drops_silently` |
| `ProcQueryAll`       |        2 | `missing_cap_returns_badcap`, `cap_present_returns_aggregate` |
| `ProcQueryLocal`     |        3 | `empty_session_returns_empty`, `returns_all_children`, `filter_specific_pids` |
| `ServiceSpawn`       |        2 | `spawn_vfs_records_thread_and_publish_cap`, `bad_payload` |
| `Escalate`           |        2 | `missing_cap`, `cap_present_grants_bundle` |
| `Shutdown`           |        2 | `missing_cap`, `shutdown_revokes_all` |
| `cap_broker::sub_mint` |   ≥3+P | `narrowing_ok`, `widening_fails`, `prop_child_subset_of_parent` |
| `RestartTracker`     |        5 | `never_policy_no_restart`, `always_policy_restarts_until_threshold`, `on_failure_only_on_nonzero`, `window_reset`, `unknown_cookie_no_restart` |
| `PgTable`            |        3 | `create_attach_detach`, `pgid_of_finds_member`, `detach_unknown_pid_idempotent` |
| `PipeRegistry`       |        3 | `create_returns_distinct_ids`, `close_known`, `close_unknown` |
| `MintGuard`          |        2 | `guard_revokes_on_drop_when_armed`, `forget_disarms_no_revoke` |
| `pid::encode/decode` |     2+P  | `encode_decode_roundtrip_smoke`, `encode_local_overflow_errors`, `prop_encode_decode_roundtrip` |

(`P` = proptest, counted as branch coverage of the property domain.)
```

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/specs/PROCMGR_CAP_REFACTOR_COVERAGE.md
git commit -m "docs: procmgr cap-refactor coverage matrix"
```

### Task 14.3: Performance ratchet check

- [ ] **Step 1: Run baseline + post**

```bash
# baseline from develop pre-refactor
git stash
git checkout develop
HARNESS_FORCE_BUILD=1 MARKER_MODE=b_spawn_warm bash scripts/harness_run.sh > /tmp/baseline.txt
git checkout procmgr-cap-refactor
git stash pop || true
HARNESS_FORCE_BUILD=1 MARKER_MODE=b_spawn_warm bash scripts/harness_run.sh > /tmp/post.txt
diff /tmp/baseline.txt /tmp/post.txt
```

- [ ] **Step 2: Compute regression %**

Manually inspect cycle counts. Acceptance: ≤ +15 %. Beyond → investigate (probable suspects: extra mint per spawn).

- [ ] **Step 3: Record numbers in commit message**

```bash
git commit --allow-empty -m "perf(procmgr): spawn warm baseline X / post Y cycles (delta Z%)"
```

### Task 14.4: Final acceptance + merge

- [ ] **Step 1: Run full pm_* suite**

```bash
for m in pm_bootstrap_two_pmgr pm_session_crash_cascade pm_cap_revoke_stale \
         pm_session_id_recycle pm_cross_session_no_leak pm_proc_query_all_cap \
         pm_pid_layout pm_service_restart; do
  HARNESS_FORCE_BUILD=1 CLUU_SHELL_AUTOSTART_CMD=$m MARKER_MODE=$m bash scripts/harness_run.sh
  grep -q "MARKER:$m:PASS" serial.log || { echo "FAIL: $m"; exit 1; }
done
echo "ALL pm_* PASS"
```

- [ ] **Step 2: Cap-purity lint clean**

```bash
cargo xtask check-cap-purity
```

Expected: PASS.

- [ ] **Step 3: Coverage gate**

```bash
cargo xtask coverage-check
```

Expected: PASS.

- [ ] **Step 4: Workspace builds clean**

```bash
cargo xtask build
cargo test --workspace --features host-test
```

Expected: PASS.

- [ ] **Step 5: Merge to `develop`**

```bash
git checkout develop
git merge --ff-only procmgr-cap-refactor
git push origin develop
```

(`--ff-only` forces a clean fast-forward; rebase locally first if necessary.)

- [ ] **Step 6: Delete legacy bypass tag + branch**

```bash
git branch -d procmgr-cap-refactor
git push origin --delete procmgr-cap-refactor
```

- [ ] **Step 7: Update memory**

Edit `~/.claude/projects/-home-vlb2bp-git-cluu/memory/project_procmgr_acl_redesign.md` and `MEMORY.md`: mark as LANDED, link to spec + plan + final commit.

- [ ] **Step 8: Final commit**

```bash
git commit --allow-empty -m "chore: procmgr cap-model refactor LANDED — see docs/superpowers/specs/2026-05-21-procmgr-cap-refactor-design.md"
git push
```

---

## Plan Self-Review (writing-plans skill checklist)

**1. Spec coverage:**
- Architecture & topology → Phase 0 + 12 (scaffold, init/login rewire).
- Scope split (root vs session) → Phases 3, 4, 7, 8, 9, 10, 11 (root handlers) + 5, 6, 7, 8, 9 (session handlers).
- PID layout 8|23 → Task 1.1 (`pid.rs`), Task 5.1 (`child_table.alloc_pid`), Task 13.2.a (`pm_pid_layout`).
- Cap broker per-session sub-mint + per-child re-mint → Task 4.1 (root cap_broker), Task 5.2 (session re-mint via Spawn).
- Generation counter → Task 3.1 (alloc/finalise), `pm_session_id_recycle` (13.2).
- SYSTEM-cap-gated proc_query_all → Task 9.2.
- Cascade teardown → Task 3.3 + `pm_session_crash_cascade`.
- Service crash + restart → Task 10.2 + `pm_service_restart`.
- MintGuard rollback → Task 2.2, exercised in 5.2 + spec §4.3.
- Stale-cap by generation → Task 3.1, `pm_cap_revoke_stale`.
- Legacy bypass deletion → Task 12.4.
- Branch big-bang → Task 0.1 + Task 14.4 ff-only merge.
- Cap-purity lint → Task 13.1.
- C1≥95 % / C2≥90 % coverage → Task 14.1 (line+branch), Task 14.2 (path matrix).
- pm_* fresh markers (legacy harness not the gate) → Phase 13.

**2. Placeholder scan:** swept; placeholder phrases only appear inside step bodies where the *file being authored* contains a placeholder that the next step replaces (e.g. `// placeholder` lines in Task 3.2's first cut, replaced in Task 4.2). No remaining `TODO`/`TBD` outside that pattern.

**3. Type consistency:** `MsgHandler::handle` signature `(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError>` reused everywhere. `Pid = i32`, `SessionId = u8`, `LocalPid = u32` consistent. `SessionEnvelope` field set identical between Task 1.3 (definition) and Task 3.2 (use). `ChildState` fields identical in Tasks 5.1 / 5.2 / 6.1 / 7.2.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-21-procmgr-cap-refactor.md`. Two execution options:

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration. Best for a refactor this size; isolates context per task.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

Pick approach.
