# Unified Spawn Protocol Implementation Plan

> **For agentic workers:** This plan is self-contained. Each step has exact file paths, complete code, and exact verification commands with expected output. Implementation target: deepseek v4 pro or equivalent. Tasks numbered sequentially; complete in order. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace six existing spawn paths (init kernel batch, procmgr autostart, SESSION_LOGIN internal spawns, PROCMGR_SPAWN, PROCMGR_CONTAINER_RUN, cluuterm posix_spawn) with one IPC verb (`PROCMGR_SPAWN_UNIFIED_LABEL = 80`) carrying a postcard-serialized `SpawnEnvelope`, plus a one-shot bootstrap verb (`PROCMGR_PRIMORDIAL_SEED_LABEL = 81`) for init handoff to procmgr.

**Architecture:** New shared crate `cluu_proto` defines `SpawnEnvelope` type and label constants. Procmgr gets one internal function `procmgr::spawn(envelope, caller_pid)` that both the IPC dispatch handler and procmgr-internal callers (autostart, SESSION_LOGIN, primordial seed) invoke. Init's kernel-side spawn path reduces to `launch_procmgr` only; all other primordials spawn via PRIMORDIAL_SEED. ViewObject becomes a procmgr-owned typed object; restart policy moves from envelope to manifest.

**Tech Stack:** Rust 2021 edition, no-std workspace, postcard 1.x for serialization (NEW dep), bitflags 2.4 (existing). Cargo workspace per `Cargo.toml`. Build via `cargo xtask build`.

**Reference spec:** `docs/superpowers/specs/2026-05-18-unified-spawn-protocol-design.md`.

---

## File Structure

### New files

- `userspace/cluu_proto/Cargo.toml` — new workspace member.
- `userspace/cluu_proto/src/lib.rs` — module root + re-exports.
- `userspace/cluu_proto/src/spawn.rs` — `SpawnEnvelope`, `ViewSource`, `FdInherit`, `FdSource`, `FdRights`, `RestartPolicy`, `SpawnReply`, `SpawnError`, label constants.
- `userspace/cluu_proto/src/primordial.rs` — `PrimordialSeed`, `PrimordialSeedReply` types.

### Modified files (in order of first touch)

- `Cargo.toml` — add `postcard` workspace dep; add `userspace/cluu_proto` member.
- `userspace/cluu_proto/Cargo.toml` — declares crate.
- `userspace/libcluu/Cargo.toml` — depend on `cluu_proto`.
- `userspace/libcluu/src/lib.rs` — re-export proto types.
- `userspace/libcluu/src/ipc.rs` — add `libcluu::spawn` public surface.
- `userspace/procmgr/Cargo.toml` — depend on `cluu_proto`.
- `userspace/procmgr/src/lib.rs` — extend with `spawn` submodule export.
- `userspace/procmgr/src/spawn.rs` (NEW) — `procmgr::spawn(envelope, caller_pid)` function + `ViewObject` table + `derive_child_view`.
- `userspace/procmgr/src/manifest_cache.rs` (NEW) — manifest loader keyed by image name.
- `userspace/procmgr/src/main.rs` — adapters for new label, retire old handlers, route autostart/SESSION_LOGIN through `procmgr::spawn`.
- `userspace/cluuterm/src/main.rs` — replace newlib `posix_spawn` + `adddup2` with `libcluu::spawn` direct call.
- `userspace/shell/src/commands/exec.rs` (path may vary; verify with grep) — pipeline and external-command spawns use `libcluu::spawn`.
- `userspace/init/src/wiring.rs` — kernel-spawn only procmgr; send PRIMORDIAL_SEED for the rest.
- `kernel/src/...` (file located by grep in Task 11) — reduce `launch_service` to `launch_procmgr`.

### Test files

- `userspace/cluu_proto/src/spawn.rs` — inline `#[cfg(test)]` mod with postcard round-trip tests.
- `userspace/procmgr/src/spawn.rs` — inline tests for view derive monotone-decrease.
- `userspace/probes/spawn_unified_smoke/` (NEW probe) — smoke test for the new verb.
- New harness markers as listed in §15 of spec 1.

---

## Build / verify commands cheat sheet

- Full build: `cargo xtask build` (expected: no errors, no warnings).
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`.
- Single crate build: `cargo build -p <crate>` (e.g., `-p cluu_proto`).
- Crate-local tests (host-side, where applicable): `cargo test -p libcluu --features host-test`.
- Boot smoke: `bash scripts/harness_run.sh` (expected: log line `compositor: ready`).
- Marker harness:
  `HARNESS_FORCE_BUILD=1 MARKER_MODE=<marker_name> bash scripts/harness_run.sh`
  Then `grep "<marker_name>: " serial.log` for PASS/FAIL.

---

## Task 1: Create `cluu_proto` crate scaffolding

**Goal:** Empty crate with `lib.rs` compiles in the workspace.

**Files:**
- Create: `userspace/cluu_proto/Cargo.toml`
- Create: `userspace/cluu_proto/src/lib.rs`
- Modify: `/home/vlb2bp/git/cluu/Cargo.toml` (workspace root; add member + postcard dep)

- [ ] **Step 1: Add postcard to workspace dependencies**

Open `/home/vlb2bp/git/cluu/Cargo.toml`. Find the `[workspace.dependencies]` section. Append:

```toml
postcard = { version = "1.0", default-features = false, features = ["alloc"] }
serde = { version = "1.0", default-features = false, features = ["alloc", "derive"] }
```

- [ ] **Step 2: Add cluu_proto to workspace members**

In the same `/home/vlb2bp/git/cluu/Cargo.toml`, inside `[workspace] members = [ ... ]`, add `"userspace/cluu_proto",` near the other userspace entries (alphabetical order; place between `"userspace/cat",` and `"userspace/cp",`).

- [ ] **Step 3: Write `userspace/cluu_proto/Cargo.toml`**

Create the file with this exact content:

```toml
[package]
name = "cluu_proto"
version = "0.1.0"
edition = "2021"
description = "CLUU wire protocol types — single source of truth for spawn, session, pts, window."
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[dependencies]
serde = { workspace = true }
postcard = { workspace = true }
bitflags = { workspace = true }

[lib]
name = "cluu_proto"
crate-type = ["rlib"]

[features]
default = []
host-test = ["serde/std", "postcard/use-std"]
```

- [ ] **Step 4: Write minimal `userspace/cluu_proto/src/lib.rs`**

Create the file:

```rust
//! CLUU wire protocol types.
//!
//! This crate is the single source of truth for IPC payload formats
//! shared between libcluu callers and service implementations
//! (procmgr, vfs, compositor, etc.).
//!
//! Specs:
//! - `docs/superpowers/specs/2026-05-18-unified-spawn-protocol-design.md`
//! - `docs/superpowers/specs/2026-05-18-terminal-pty-unification-design.md`
//! - `docs/superpowers/specs/2026-05-18-session-lifecycle-design.md`
//! - `docs/superpowers/specs/2026-05-18-window-protocol-design.md`

#![cfg_attr(not(feature = "host-test"), no_std)]

extern crate alloc;

pub mod spawn;
pub mod primordial;

pub use spawn::*;
pub use primordial::*;

/// ABI version stamped into `words[1]` of every wire message.
pub const ABI_VERSION: u32 = 1;

/// Caller-side token handle width (matches libcluu/procmgr handle ABI).
pub type TokenHandle = u64;
```

- [ ] **Step 5: Stub the two submodule files so the crate compiles**

Create `userspace/cluu_proto/src/spawn.rs` with placeholder:

```rust
//! Spawn protocol types — see spec 1.
```

Create `userspace/cluu_proto/src/primordial.rs` with placeholder:

```rust
//! Primordial seed types — see spec 1 §13.
```

- [ ] **Step 6: Build the workspace**

Run: `cd /home/vlb2bp/git/cluu && cargo build -p cluu_proto`

Expected output (last line): `Finished dev` profile, no errors.

If `postcard` resolution fails, check that step 1's workspace deps were added inside the `[workspace.dependencies]` block (not the top-level `[dependencies]` — there shouldn't be one at the workspace root).

- [ ] **Step 7: Commit**

```bash
cd /home/vlb2bp/git/cluu
git add Cargo.toml userspace/cluu_proto/
git commit -m "feat(cluu_proto): scaffold shared wire-protocol crate"
```

---

## Task 2: Implement `SpawnEnvelope` types

**Goal:** Define every type referenced by the spawn verb. Round-trip postcard encode/decode tests.

**Files:**
- Modify: `userspace/cluu_proto/src/spawn.rs`

- [ ] **Step 1: Replace `userspace/cluu_proto/src/spawn.rs` with full type definitions**

```rust
//! Spawn protocol types — see spec 1.
//!
//! Wire envelope for `PROCMGR_SPAWN_UNIFIED_LABEL = 80`.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::TokenHandle;

/// Wire label for the unified spawn IPC verb.
pub const PROCMGR_SPAWN_UNIFIED_LABEL: u32 = 80;

/// One spawn call's payload. Postcard-serialized into the IPC payload buffer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpawnEnvelope {
    /// Image name (1:1 with manifest under `/var/images/<image>/manifest.toml`).
    pub image: String,

    /// argv list. Procmgr overrides `args[0]` with `basename(manifest.entrypoint)`
    /// regardless of value (process-identity rule, spec 1 §6).
    pub args: Vec<String>,

    /// Environment as (key, value) pairs. Newlib `posix_spawn` shim joins to
    /// `KEY=VAL` for the C runtime.
    pub env: Vec<(String, String)>,

    /// View source (parent-derive or bootstrap-root for init primordials).
    pub view: ViewSource,

    /// FD inheritance manifest — sole fd-wiring mechanism on the wire.
    pub fd_inherit: Vec<FdInherit>,

    /// Optional session-token cap. `None` permitted only for sessionless
    /// callers (init, procmgr-internal, or manifests declaring
    /// `RIGHT_SESSIONLESS_SPAWN`).
    pub session: Option<TokenHandle>,

    /// Optional notify endpoint cap. `None` = silent exit; otherwise
    /// procmgr derives IPC_SEND into its own table and fires PROC_EXIT_LABEL
    /// on child exit.
    pub notify: Option<TokenHandle>,
}

/// View origin discriminator. Steady-state uses `Derive`; init bootstrap uses
/// `BootstrapRoot` (rejected for any caller other than init's pid during the
/// one-shot primordial seed handler).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ViewSource {
    Derive(TokenHandle),
    BootstrapRoot,
}

/// One fd-inheritance entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FdInherit {
    /// Slot in the child's fd table.
    pub child_fd: u32,
    /// Where the inherited fd comes from.
    pub source: FdSource,
    /// Rights subset — must be ≤ caller's rights on the source fd.
    pub rights: FdRights,
}

/// Where the inherited fd lives. Currently VFS-backed only.
/// Extending to `PipeCap` / `EndpointCap` later is additive.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FdSource {
    VfsFd {
        vfs_client_id: u64,
        vfs_remote_fd: u32,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct FdRights {
    pub read: bool,
    pub write: bool,
}

impl FdRights {
    pub const READ_ONLY: Self = Self { read: true, write: false };
    pub const WRITE_ONLY: Self = Self { read: false, write: true };
    pub const READ_WRITE: Self = Self { read: true, write: true };

    pub fn is_subset_of(self, other: Self) -> bool {
        (!self.read || other.read) && (!self.write || other.write)
    }
}

/// Restart policy. Lives in this crate so manifest parsing and procmgr storage
/// share the type, but is NOT a `SpawnEnvelope` field — manifest is the source
/// of truth per spec 1 §11.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RestartPolicy {
    Never,
    Always,
    OnFailure { max: u32, window_ms: u64 },
}

impl Default for RestartPolicy {
    fn default() -> Self {
        RestartPolicy::Never
    }
}

/// Successful reply from `procmgr::spawn`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpawnReply {
    pub pid: u32,
    pub child_thread_token: TokenHandle,
}

/// All ways spawn can fail.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SpawnError {
    /// `envelope.image` not found in `/var/images/`.
    ImageNotFound,
    /// Manifest read but parse failed; payload is the parse error message.
    ManifestInvalid(String),
    /// View token resolution failed, OR derive would widen.
    ViewDeriveDenied,
    /// FD inheritance failed at the given child_fd index.
    FdInheritDeniedAt(u32),
    /// Session token resolution failed (revoked / dying).
    SessionRevoked,
    /// Notify token resolution failed.
    NotifyTokenInvalid,
    /// Caller's manifest does not declare the rights to spawn.
    PermissionDenied,
    /// Kernel resource exhaustion (Space alloc, Thread alloc).
    OutOfMemory,
    /// Diagnostic. Should be rare; if seen, file a bug.
    Internal(u32),
}
```

- [ ] **Step 2: Add round-trip tests at the bottom of the same file**

Append to `userspace/cluu_proto/src/spawn.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn sample_envelope() -> SpawnEnvelope {
        SpawnEnvelope {
            image: String::from("shell"),
            args: vec![String::from("shell"), String::from("-c"), String::from("echo hi")],
            env: vec![
                (String::from("HOME"), String::from("/home/dave")),
                (String::from("TERM"), String::from("xterm-256color")),
            ],
            view: ViewSource::Derive(0xDEAD_BEEF_u64),
            fd_inherit: vec![FdInherit {
                child_fd: 0,
                source: FdSource::VfsFd { vfs_client_id: 7, vfs_remote_fd: 3 },
                rights: FdRights::READ_ONLY,
            }],
            session: Some(0xCAFE_F00D_u64),
            notify: Some(0xFACE_BEEF_u64),
        }
    }

    #[test]
    fn envelope_roundtrip() {
        let env = sample_envelope();
        let bytes = postcard::to_allocvec(&env).expect("serialize");
        let decoded: SpawnEnvelope = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(decoded.image, env.image);
        assert_eq!(decoded.args, env.args);
        assert_eq!(decoded.env, env.env);
        assert_eq!(decoded.fd_inherit.len(), env.fd_inherit.len());
        assert_eq!(decoded.session, env.session);
        assert_eq!(decoded.notify, env.notify);
    }

    #[test]
    fn fd_rights_subset() {
        let ro = FdRights::READ_ONLY;
        let rw = FdRights::READ_WRITE;
        assert!(ro.is_subset_of(rw));
        assert!(!rw.is_subset_of(ro));
        assert!(ro.is_subset_of(ro));
    }

    #[test]
    fn spawn_error_roundtrip() {
        let err = SpawnError::FdInheritDeniedAt(2);
        let bytes = postcard::to_allocvec(&err).expect("serialize");
        let decoded: SpawnError = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(decoded, err);
    }

    #[test]
    fn bootstrap_root_roundtrip() {
        let env = SpawnEnvelope {
            view: ViewSource::BootstrapRoot,
            ..sample_envelope()
        };
        let bytes = postcard::to_allocvec(&env).expect("serialize");
        let decoded: SpawnEnvelope = postcard::from_bytes(&bytes).expect("deserialize");
        match decoded.view {
            ViewSource::BootstrapRoot => (),
            _ => panic!("expected BootstrapRoot"),
        }
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cd /home/vlb2bp/git/cluu && cargo test -p cluu_proto --features host-test`

Expected: 4 tests pass.

If `cargo test` fails with "can't find crate for `std`", verify `host-test` feature is properly gated in `Cargo.toml` (Task 1 Step 3 included `host-test = ["serde/std", "postcard/use-std"]`). Re-check.

- [ ] **Step 4: Commit**

```bash
cd /home/vlb2bp/git/cluu
git add userspace/cluu_proto/src/spawn.rs
git commit -m "feat(cluu_proto): SpawnEnvelope + round-trip tests"
```

---

## Task 3: Implement `PrimordialSeed` types

**Files:**
- Modify: `userspace/cluu_proto/src/primordial.rs`

- [ ] **Step 1: Write `primordial.rs`**

Replace the file content:

```rust
//! Primordial seed types — see spec 1 §13.
//!
//! Wire format for `PROCMGR_PRIMORDIAL_SEED_LABEL = 81`. Init sends this
//! one-shot message to procmgr immediately after procmgr's kernel-spawn.
//! Procmgr rejects the call after first success and rejects any caller
//! other than init's pid.

use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::spawn::{SpawnEnvelope, SpawnError, SpawnReply};

pub const PROCMGR_PRIMORDIAL_SEED_LABEL: u32 = 81;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrimordialSeed {
    pub primordials: Vec<SpawnEnvelope>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrimordialSeedReply {
    /// One result per envelope in the request, in input order.
    pub results: Vec<Result<SpawnReply, SpawnError>>,
}
```

- [ ] **Step 2: Add a smoke test at the bottom**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::{FdInherit, FdRights, FdSource, SpawnEnvelope, SpawnReply, ViewSource};
    use alloc::{string::String, vec};

    #[test]
    fn primordial_seed_roundtrip() {
        let seed = PrimordialSeed {
            primordials: vec![SpawnEnvelope {
                image: String::from("registry"),
                args: vec![],
                env: vec![],
                view: ViewSource::BootstrapRoot,
                fd_inherit: Vec::new(),
                session: None,
                notify: None,
            }],
        };
        let bytes = postcard::to_allocvec(&seed).expect("serialize");
        let decoded: PrimordialSeed = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(decoded.primordials.len(), 1);
        assert_eq!(decoded.primordials[0].image, "registry");
    }

    #[test]
    fn reply_roundtrip() {
        let reply = PrimordialSeedReply {
            results: vec![
                Ok(SpawnReply { pid: 2, child_thread_token: 0x1000 }),
                Err(SpawnError::ImageNotFound),
            ],
        };
        let bytes = postcard::to_allocvec(&reply).expect("serialize");
        let decoded: PrimordialSeedReply = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(decoded.results.len(), 2);
        match &decoded.results[0] {
            Ok(r) => assert_eq!(r.pid, 2),
            Err(_) => panic!("expected Ok"),
        }
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cd /home/vlb2bp/git/cluu && cargo test -p cluu_proto --features host-test`

Expected: 6 tests pass (4 from Task 2 + 2 new).

- [ ] **Step 4: Commit**

```bash
git add userspace/cluu_proto/src/primordial.rs
git commit -m "feat(cluu_proto): PrimordialSeed + round-trip tests"
```

---

## Task 4: Wire `cluu_proto` into `libcluu`

**Goal:** libcluu re-exports proto types. No call-site changes yet.

**Files:**
- Modify: `userspace/libcluu/Cargo.toml`
- Modify: `userspace/libcluu/src/lib.rs`

- [ ] **Step 1: Add `cluu_proto` dependency to libcluu**

Open `userspace/libcluu/Cargo.toml`. In the `[dependencies]` section, after the existing `bitflags`, `klibcluu`, `spin`, `lazy_static` lines, add:

```toml
cluu_proto = { path = "../cluu_proto" }
```

- [ ] **Step 2: Re-export proto types from libcluu**

Open `userspace/libcluu/src/lib.rs`. Find a logical place near the existing `pub mod` declarations (e.g., right after the crate's existing public modules). Add:

```rust
/// Re-export of `cluu_proto` — the wire-protocol types crate.
///
/// Callers may use `libcluu::proto::SpawnEnvelope` or import directly from
/// `cluu_proto`; both paths reach the same types.
pub use cluu_proto as proto;
```

- [ ] **Step 3: Verify build**

Run: `cd /home/vlb2bp/git/cluu && cargo build -p libcluu`

Expected: builds clean. If linker errors mention duplicate symbols, libcluu may already have a `proto` module — rename the re-export to `pub use cluu_proto as cluu_proto_types;` and fix Step 2 accordingly.

- [ ] **Step 4: Commit**

```bash
git add userspace/libcluu/Cargo.toml userspace/libcluu/src/lib.rs
git commit -m "feat(libcluu): re-export cluu_proto"
```

---

## Task 5: Wire `cluu_proto` into `procmgr`

**Files:**
- Modify: `userspace/procmgr/Cargo.toml`
- Modify: `userspace/procmgr/src/lib.rs`

- [ ] **Step 1: Add dependency**

Open `userspace/procmgr/Cargo.toml`. In `[dependencies]`, add:

```toml
cluu_proto = { path = "../cluu_proto" }
```

(Add `postcard = { workspace = true }` here too if not already imported via `libcluu`.)

- [ ] **Step 2: Re-export from lib.rs**

Open `userspace/procmgr/src/lib.rs`. Near the top (after any existing `pub mod` lines), add:

```rust
pub use cluu_proto as proto;
```

- [ ] **Step 3: Build**

Run: `cd /home/vlb2bp/git/cluu && cargo build -p procmgr`

Expected: builds clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/Cargo.toml userspace/procmgr/src/lib.rs
git commit -m "feat(procmgr): depend on cluu_proto"
```

---

## Task 6: Procmgr-internal manifest cache

**Goal:** A `ManifestCache` struct that loads `/var/images/<image>/manifest.toml` on first miss, parses out `entrypoint` and `restart_policy`, caches by image name. Used by `procmgr::spawn`.

**Files:**
- Create: `userspace/procmgr/src/manifest_cache.rs`
- Modify: `userspace/procmgr/src/lib.rs` (add module decl)

**Note for the engineer:** Procmgr already reads manifests inside `handle_container_run`. Locate that code first with `grep -n "manifest" userspace/procmgr/src/main.rs | head -20`. Reuse its TOML parser. This task only adds the per-image cache layer and exposes a clean API; it does NOT rewrite the parser.

- [ ] **Step 1: Inspect existing manifest-reading code**

Run: `cd /home/vlb2bp/git/cluu && grep -n "manifest\|Cluufile" userspace/procmgr/src/main.rs | head -30`

Note the file:line of the existing parse function (likely something like `parse_manifest` or `load_image_manifest`). You will reuse it.

- [ ] **Step 2: Write `userspace/procmgr/src/manifest_cache.rs`**

```rust
//! Per-image manifest cache.
//!
//! Holds the parsed Cluufile state keyed by image name. Lazily populated
//! on first miss via the existing manifest-reading helper.

use alloc::collections::BTreeMap;
use alloc::string::String;
use spin::Mutex;

use cluu_proto::spawn::RestartPolicy;

/// Cached projection of a Cluufile manifest.
#[derive(Clone, Debug)]
pub struct CachedManifest {
    /// Full path to the entrypoint binary, e.g., "/bin/shell".
    pub entrypoint: String,
    /// Restart policy declared by the Cluufile (defaults to Never if absent).
    pub restart_policy: RestartPolicy,
    /// Whether the manifest grants `RIGHT_SESSIONLESS_SPAWN`.
    pub allow_sessionless: bool,
}

pub struct ManifestCache {
    inner: Mutex<BTreeMap<String, CachedManifest>>,
}

impl ManifestCache {
    pub const fn new() -> Self {
        Self { inner: Mutex::new(BTreeMap::new()) }
    }

    /// Look up by image name. On miss, calls `loader` (which must read the
    /// manifest from VFS and build a `CachedManifest`). Returns `None` if
    /// the loader fails (image not found, parse error).
    pub fn get_or_load<F>(&self, image: &str, loader: F) -> Option<CachedManifest>
    where
        F: FnOnce() -> Option<CachedManifest>,
    {
        {
            let guard = self.inner.lock();
            if let Some(m) = guard.get(image) {
                return Some(m.clone());
            }
        }
        let loaded = loader()?;
        let mut guard = self.inner.lock();
        guard.entry(image.into()).or_insert(loaded.clone());
        Some(loaded)
    }

    /// Invalidate one entry (used when an image is reinstalled).
    pub fn invalidate(&self, image: &str) {
        self.inner.lock().remove(image);
    }
}

/// Singleton instance. Procmgr's main module holds the loader closure.
pub static MANIFEST_CACHE: ManifestCache = ManifestCache::new();
```

- [ ] **Step 3: Declare the module**

In `userspace/procmgr/src/lib.rs`, add:

```rust
pub mod manifest_cache;
```

- [ ] **Step 4: Build**

Run: `cd /home/vlb2bp/git/cluu && cargo build -p procmgr`

Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/lib.rs userspace/procmgr/src/manifest_cache.rs
git commit -m "feat(procmgr): manifest cache scaffolding"
```

---

## Task 7: Procmgr-internal `ViewObject` table

**Goal:** Replace today's implicit envelope-based view tracking with a typed `ViewObject` table inside procmgr. Each entry has a parent pointer, mount list, refcount.

**Files:**
- Create: `userspace/procmgr/src/view_table.rs`
- Modify: `userspace/procmgr/src/lib.rs` (module decl)

**Note for the engineer:** Mount state in procmgr today is in `mount_policy.rs` and `envelopes.rs`. You're not deleting those; you're adding a typed-object wrapper that *uses* the parsed mount data those modules produce. `narrow_for_manifest` is the existing function that filters mounts per a Cluufile's MOUNT directives — locate it first via `grep`.

- [ ] **Step 1: Locate the existing mount-narrowing function**

Run: `cd /home/vlb2bp/git/cluu && grep -rn "narrow_for_manifest\|mount_policy\|MountPolicy" userspace/procmgr/src/ | head -10`

Note the function name and location. If a function with that exact name doesn't exist, look for the closest analogue (likely something like `mount_policy::apply` or `derive_mounts_for_image`).

- [ ] **Step 2: Write `userspace/procmgr/src/view_table.rs`**

```rust
//! Procmgr-owned ViewObject table.
//!
//! A `ViewObject` represents the VFS view a process sees. Each carries
//! a parent pointer (for derive chains), a list of mounts, and a refcount
//! tracking how many tokens reference it. Spec 1 §8.

use alloc::vec::Vec;
use spin::Mutex;

use cluu_proto::spawn::SpawnError;

pub type ViewObjectId = u32;

/// One mount entry inside a view.
#[derive(Clone, Debug)]
pub struct MountEntry {
    pub path: alloc::string::String,
    pub rights: MountRights,
    /// Backend reference (memfs id, ext2 path, devfs marker, etc.).
    /// Stored as opaque bytes; interpretation is procmgr's mount-policy
    /// concern, not this module's.
    pub backend: alloc::vec::Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MountRights {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl MountRights {
    pub fn is_subset_of(self, other: Self) -> bool {
        (!self.read || other.read)
            && (!self.write || other.write)
            && (!self.execute || other.execute)
    }
}

#[derive(Clone, Debug)]
pub struct ViewObject {
    pub id: ViewObjectId,
    pub parent: Option<ViewObjectId>,
    pub mounts: Vec<MountEntry>,
    pub refcount: u32,
}

pub struct ViewTable {
    inner: Mutex<ViewTableInner>,
}

struct ViewTableInner {
    next_id: ViewObjectId,
    entries: alloc::collections::BTreeMap<ViewObjectId, ViewObject>,
}

impl ViewTable {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(ViewTableInner {
                next_id: 1,
                entries: alloc::collections::BTreeMap::new(),
            }),
        }
    }

    pub fn insert(&self, parent: Option<ViewObjectId>, mounts: Vec<MountEntry>) -> ViewObjectId {
        let mut g = self.inner.lock();
        let id = g.next_id;
        g.next_id = g.next_id.wrapping_add(1);
        g.entries.insert(id, ViewObject { id, parent, mounts, refcount: 1 });
        id
    }

    pub fn inc_ref(&self, id: ViewObjectId) -> Result<(), SpawnError> {
        let mut g = self.inner.lock();
        let e = g.entries.get_mut(&id).ok_or(SpawnError::ViewDeriveDenied)?;
        e.refcount = e.refcount.saturating_add(1);
        Ok(())
    }

    pub fn dec_ref(&self, id: ViewObjectId) {
        let mut g = self.inner.lock();
        if let Some(e) = g.entries.get_mut(&id) {
            e.refcount = e.refcount.saturating_sub(1);
            if e.refcount == 0 {
                g.entries.remove(&id);
            }
        }
    }

    pub fn snapshot(&self, id: ViewObjectId) -> Option<ViewObject> {
        self.inner.lock().entries.get(&id).cloned()
    }
}

pub static VIEW_TABLE: ViewTable = ViewTable::new();

/// Monotone-decrease check: every entry in `child_mounts` must be a
/// narrower-or-equal subset of some entry in `parent_mounts` (same path
/// prefix, rights ≤ parent's).
pub fn verify_monotone(child_mounts: &[MountEntry], parent_mounts: &[MountEntry])
    -> Result<(), SpawnError>
{
    for cm in child_mounts {
        let matched = parent_mounts.iter().find(|pm| cm.path.starts_with(pm.path.as_str()));
        match matched {
            None => return Err(SpawnError::ViewDeriveDenied),
            Some(pm) => {
                if !cm.rights.is_subset_of(pm.rights) {
                    return Err(SpawnError::ViewDeriveDenied);
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Add module decl**

In `userspace/procmgr/src/lib.rs`:

```rust
pub mod view_table;
```

- [ ] **Step 4: Add monotone-decrease unit test**

At the bottom of `userspace/procmgr/src/view_table.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn mk_mount(path: &str, r: bool, w: bool) -> MountEntry {
        MountEntry {
            path: alloc::string::String::from(path),
            rights: MountRights { read: r, write: w, execute: false },
            backend: alloc::vec::Vec::new(),
        }
    }

    #[test]
    fn child_narrower_rights_accepted() {
        let parent = vec![mk_mount("/home", true, true)];
        let child = vec![mk_mount("/home", true, false)];
        assert!(verify_monotone(&child, &parent).is_ok());
    }

    #[test]
    fn child_wider_rights_rejected() {
        let parent = vec![mk_mount("/home", true, false)];
        let child = vec![mk_mount("/home", true, true)];
        assert!(verify_monotone(&child, &parent).is_err());
    }

    #[test]
    fn child_unknown_path_rejected() {
        let parent = vec![mk_mount("/home", true, true)];
        let child = vec![mk_mount("/etc", true, false)];
        assert!(verify_monotone(&child, &parent).is_err());
    }

    #[test]
    fn child_subpath_accepted() {
        let parent = vec![mk_mount("/home", true, true)];
        let child = vec![mk_mount("/home/dave", true, true)];
        assert!(verify_monotone(&child, &parent).is_ok());
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cd /home/vlb2bp/git/cluu && cargo test -p procmgr --features host-test 2>&1 | tail -20`

If procmgr doesn't expose a `host-test` feature, host-test these from `cluu_proto` instead — copy the test module into a temporary scratch crate for verification, or skip and rely on the full build to catch issues. Document this in the commit message if you skip.

Expected (if testable): 4 view-table tests pass.

- [ ] **Step 6: Commit**

```bash
git add userspace/procmgr/src/lib.rs userspace/procmgr/src/view_table.rs
git commit -m "feat(procmgr): ViewObject table + monotone-derive check"
```

---

## Task 8: Procmgr-internal `spawn` function

**Goal:** One function `procmgr::spawn(envelope, caller_pid)` that does the whole 10-step body of spec 1 §12. Both the new IPC handler and procmgr-internal callers (autostart, SESSION_LOGIN) will call it.

**Files:**
- Create: `userspace/procmgr/src/spawn.rs`
- Modify: `userspace/procmgr/src/lib.rs`

**Note for the engineer:** This function is a careful refactor of the existing `handle_spawn_message` (around `userspace/procmgr/src/main.rs:4419`) and `handle_container_run` (around `userspace/procmgr/src/main.rs:5482`). Read both before starting. Your goal: extract the common spawn logic into `procmgr::spawn`; leave both `handle_*` functions in place for now (Task 9 makes them adapters; Task 16 deletes them).

- [ ] **Step 1: Read the existing spawn handlers**

Run:
```
grep -n "fn handle_spawn_message\|fn handle_container_run" userspace/procmgr/src/main.rs
```

Note the line ranges. Read both functions end-to-end. Identify:
- Where they resolve view (typically reads caller's profile, applies mount-policy narrowing).
- Where they resolve session (typically inside `handle_container_run`).
- Where they derive notify-cap (around `resolve_notify_endpoint`, commit `a597e09`).
- Where they install FdInherit entries.
- Where they allocate Space + Thread.
- Where they load ELF.
- Where they write ProcessInfo page.
- Where they insert ProcessEntry + start the thread.

These are the same steps your new `procmgr::spawn` will perform. You're not changing semantics; you're consolidating into one function with the new `SpawnEnvelope` input type.

- [ ] **Step 2: Write `userspace/procmgr/src/spawn.rs`**

```rust
//! Procmgr-internal spawn function.
//!
//! `spawn(envelope, caller_pid)` is the single entry point called by:
//! - the unified IPC dispatch handler (Task 9)
//! - procmgr autostart (Task 14)
//! - SESSION_LOGIN internal spawns (Task 15)
//! - the PRIMORDIAL_SEED handler (Task 12)
//!
//! 10-step body per spec 1 §12.

use alloc::string::String;
use alloc::vec::Vec;

use cluu_proto::spawn::{
    FdInherit, FdSource, RestartPolicy, SpawnEnvelope, SpawnError, SpawnReply, ViewSource,
};
use cluu_proto::TokenHandle;

use crate::manifest_cache::{CachedManifest, MANIFEST_CACHE};
use crate::view_table::{verify_monotone, ViewObjectId, VIEW_TABLE};

/// Caller-side hook the engineer wires once the function lives inside
/// procmgr's main: each helper below currently exists in `main.rs` under
/// a slightly different name. The engineer points each call at the
/// matching existing helper or moves the helper into a private module.
mod hooks {
    use cluu_proto::TokenHandle;

    /// Resolve a token in caller_pid's table → procmgr-side raw endpoint.
    /// Returns None if invalid / revoked. Matches behavior of the existing
    /// `resolve_caller_token` helper in procmgr/main.rs.
    pub fn resolve_token(_token: TokenHandle, _caller_pid: u32) -> Option<u64> {
        unimplemented!("wire to existing procmgr token-resolution helper")
    }

    /// Derive an IPC_SEND cap on the resolved endpoint into procmgr's own
    /// token table. Matches `resolve_notify_endpoint` from commit a597e09.
    pub fn derive_send(_raw_endpoint: u64) -> Option<TokenHandle> {
        unimplemented!("wire to existing notify_endpoint_derive helper")
    }

    /// VFS-side: derive a child fd token from (parent_cid, parent_fd) into
    /// the child thread's fd slot. Returns the derived child-side handle
    /// for procmgr's bookkeeping.
    pub fn vfs_derive_child_fd(
        _vfs_client_id: u64,
        _vfs_remote_fd: u32,
        _child_tid: u64,
        _child_fd: u32,
    ) -> Result<TokenHandle, ()> {
        unimplemented!("wire to existing vfs_derive_child_fd")
    }

    /// Allocate a fresh child Space + initial Thread, return both.
    pub fn alloc_child_space_and_thread() -> Result<(u64 /* space */, u64 /* thread tid */), ()> {
        unimplemented!("wire to existing space-and-thread allocator")
    }

    /// Map the image's ELF into the child's space. Returns entry point address.
    pub fn load_elf(_image: &str, _space: u64) -> Result<u64 /* entry */, ()> {
        unimplemented!("wire to existing ELF loader")
    }

    /// Write a `ProcessInfo` page into the child's address space carrying
    /// argv / env / inherited-fd table. Existing helper already exists; reuse.
    pub fn write_process_info(
        _space: u64,
        _argv: &[alloc::string::String],
        _env: &[(alloc::string::String, alloc::string::String)],
        _inherited_fds: &[(u32 /* child_fd */, u64 /* vfs_cid */, u32 /* vfs_fd */)],
    ) -> Result<(), ()> {
        unimplemented!("wire to existing process_info_page writer")
    }

    /// Insert a ProcessEntry into procmgr's table; returns assigned pid.
    /// `restart_policy` and `restart_envelope` are stored for replay (spec 1 §11).
    pub fn insert_process_entry(
        _tid: u64,
        _space: u64,
        _image: &str,
        _comm: &str,
        _parent_pid: u32,
        _session_id: Option<u32>,
        _view_id: u32,
        _notify: Option<TokenHandle>,
        _restart_policy: cluu_proto::spawn::RestartPolicy,
        _restart_envelope: cluu_proto::spawn::SpawnEnvelope,
    ) -> Result<u32 /* pid */, ()> {
        unimplemented!("wire to existing process-table insert")
    }

    /// Start the suspended thread (existing helper).
    pub fn resume_thread(_tid: u64) -> Result<(), ()> {
        unimplemented!("wire to existing thread-resume helper")
    }

    /// Derive a thread-token in caller_pid's table referencing the new child's
    /// thread. Returns the caller-side handle. Existing helper.
    pub fn derive_thread_token_for_caller(
        _child_tid: u64,
        _caller_pid: u32,
    ) -> Result<TokenHandle, ()> {
        unimplemented!("wire to existing thread-token derive helper")
    }

    /// Resolve a SessionObject by token; check it is Live; bump refcount.
    /// Returns the session_id.
    pub fn resolve_session_token(
        _token: TokenHandle,
        _caller_pid: u32,
    ) -> Result<u32 /* session_id */, ()> {
        unimplemented!("wire to session_table.resolve")
    }

    /// Decrement a session's refcount (rollback path).
    pub fn dec_session_refcount(_session_id: u32) {
        unimplemented!("wire to session_table.dec_ref")
    }

    /// Revoke a token in procmgr's own table (rollback path for notify cap).
    pub fn revoke_procmgr_token(_token: TokenHandle) {
        unimplemented!("wire to existing token_revoke helper")
    }

    /// Tear down a partially-built child space (rollback path).
    pub fn destroy_space(_space: u64) {
        unimplemented!("wire to invoke_space_destroy")
    }

    /// Check whether caller_pid's manifest allows sessionless spawn.
    pub fn caller_can_spawn_sessionless(_caller_pid: u32) -> bool {
        unimplemented!("wire to manifest right-check")
    }

    /// Init's pid (constant after boot).
    pub fn init_pid() -> u32 {
        unimplemented!("return procmgr's stored init_pid")
    }

    /// Build the per-image root view (only callable during PRIMORDIAL_SEED).
    pub fn build_root_view_for_primordial(_manifest: &super::CachedManifest) -> Result<super::ViewObjectId, ()> {
        unimplemented!("wire to root-view builder")
    }

    /// Derive a child view from `parent_view_id`, narrowed per `manifest`'s
    /// MOUNT directives. Returns the new child ViewObjectId.
    pub fn narrow_for_manifest(
        _parent_view_id: super::ViewObjectId,
        _manifest: &super::CachedManifest,
    ) -> Result<super::ViewObjectId, ()> {
        unimplemented!("wire to existing mount-policy narrowing")
    }
}

/// True if `caller_pid` is the init pid or the procmgr itself (in-process call).
fn is_system_caller(caller_pid: u32, procmgr_self_pid: u32) -> bool {
    caller_pid == hooks::init_pid() || caller_pid == procmgr_self_pid
}

/// Procmgr-self pid (procmgr's own pid). For now, a constant lookup; the engineer
/// wires this to whatever procmgr already uses to refer to itself.
fn procmgr_self_pid() -> u32 {
    // TODO when integrating: wire to existing procmgr-self-pid helper.
    0
}

/// The single spawn entry point. Returns Ok(SpawnReply) on success or
/// Err(SpawnError) with a concrete discriminant on failure. No timeouts;
/// no waits on the child.
pub fn spawn(envelope: SpawnEnvelope, caller_pid: u32) -> Result<SpawnReply, SpawnError> {
    // Step 1: deserialize already happened at the IPC boundary; envelope is in.

    // Step 2: load manifest from cache (or VFS on miss).
    let manifest = MANIFEST_CACHE
        .get_or_load(&envelope.image, || load_manifest_from_vfs(&envelope.image))
        .ok_or(SpawnError::ImageNotFound)?;

    // Compute the process identity (basename of entrypoint), spec 1 §6.
    let comm = basename(&manifest.entrypoint).into();

    // Override argv[0] with comm (spec 1 §6).
    let mut argv = envelope.args.clone();
    if argv.is_empty() {
        argv.push(String::from(&comm));
    } else {
        argv[0] = String::from(&comm);
    }

    // Step 3: resolve & derive caps.
    let mut rollback = RollbackList::default();

    let view_id = match &envelope.view {
        ViewSource::Derive(parent_token) => {
            let parent_view_id = resolve_view_token(*parent_token, caller_pid)?;
            let child_view = hooks::narrow_for_manifest(parent_view_id, &manifest)
                .map_err(|_| SpawnError::ViewDeriveDenied)?;
            // Monotone-decrease check happens inside narrow_for_manifest; the
            // engineer must ensure that helper rejects widening with the
            // appropriate error. If it doesn't, wrap with verify_monotone here.
            rollback.view = Some(child_view);
            child_view
        }
        ViewSource::BootstrapRoot => {
            // Only init may use BootstrapRoot, and only while the primordial-seed
            // handler is active (the seed handler sets a flag the engineer must
            // wire in Task 12). Procmgr enforces that here.
            if caller_pid != hooks::init_pid() {
                return Err(SpawnError::ViewDeriveDenied);
            }
            let v = hooks::build_root_view_for_primordial(&manifest)
                .map_err(|_| SpawnError::ViewDeriveDenied)?;
            rollback.view = Some(v);
            v
        }
    };

    let session_id = match envelope.session {
        None => {
            if !is_system_caller(caller_pid, procmgr_self_pid())
                && !manifest.allow_sessionless
                && !hooks::caller_can_spawn_sessionless(caller_pid)
            {
                rollback_all(rollback);
                return Err(SpawnError::PermissionDenied);
            }
            None
        }
        Some(t) => {
            let sid = hooks::resolve_session_token(t, caller_pid)
                .map_err(|_| {
                    rollback_all(rollback.clone());
                    SpawnError::SessionRevoked
                })?;
            rollback.session_id = Some(sid);
            Some(sid)
        }
    };

    let notify_derived = match envelope.notify {
        None => None,
        Some(t) => {
            let raw = hooks::resolve_token(t, caller_pid).ok_or_else(|| {
                rollback_all(rollback.clone());
                SpawnError::NotifyTokenInvalid
            })?;
            let derived = hooks::derive_send(raw).ok_or_else(|| {
                rollback_all(rollback.clone());
                SpawnError::NotifyTokenInvalid
            })?;
            rollback.notify_token = Some(derived);
            Some(derived)
        }
    };

    // Step 4: allocate Space + initial Thread.
    let (space, child_tid) = hooks::alloc_child_space_and_thread().map_err(|_| {
        rollback_all(rollback.clone());
        SpawnError::OutOfMemory
    })?;
    rollback.space = Some(space);

    // Step 5: install fd_inherit entries.
    let mut inherited_for_pi: Vec<(u32, u64, u32)> = Vec::with_capacity(envelope.fd_inherit.len());
    for entry in &envelope.fd_inherit {
        match &entry.source {
            FdSource::VfsFd { vfs_client_id, vfs_remote_fd } => {
                hooks::vfs_derive_child_fd(*vfs_client_id, *vfs_remote_fd, child_tid, entry.child_fd)
                    .map_err(|_| {
                        rollback_all(rollback.clone());
                        SpawnError::FdInheritDeniedAt(entry.child_fd)
                    })?;
                rollback.installed_fds.push(entry.child_fd);
                inherited_for_pi.push((entry.child_fd, *vfs_client_id, *vfs_remote_fd));
            }
        }
    }

    // Step 6: load ELF.
    let _entry = hooks::load_elf(&envelope.image, space).map_err(|_| {
        rollback_all(rollback.clone());
        SpawnError::ImageNotFound // ELF load failure mirrors image-not-found from caller's PoV
    })?;

    // Step 7: write ProcessInfo page.
    hooks::write_process_info(space, &argv, &envelope.env, &inherited_for_pi).map_err(|_| {
        rollback_all(rollback.clone());
        SpawnError::Internal(0xE_BADPI)
    })?;

    // Step 8: insert ProcessEntry — first non-rollback-able step.
    let pid = hooks::insert_process_entry(
        child_tid,
        space,
        &envelope.image,
        &comm,
        caller_pid,
        session_id,
        view_id,
        notify_derived,
        manifest.restart_policy,
        envelope.clone(),
    )
    .map_err(|_| {
        rollback_all(rollback.clone());
        SpawnError::Internal(0xE_PROCTAB)
    })?;
    rollback.process_entry_pid = Some(pid);

    // Step 9: start the thread.
    hooks::resume_thread(child_tid).map_err(|_| {
        // ProcessEntry already inserted; clean up by issuing destroy.
        // The engineer wires the destroy path; for now, treat as Internal.
        SpawnError::Internal(0xE_RESUME)
    })?;

    // Step 10: derive a thread token for the caller.
    let child_thread_token = hooks::derive_thread_token_for_caller(child_tid, caller_pid)
        .map_err(|_| SpawnError::Internal(0xE_TOK))?;

    Ok(SpawnReply { pid, child_thread_token })
}

fn resolve_view_token(token: TokenHandle, caller_pid: u32) -> Result<ViewObjectId, SpawnError> {
    // The engineer wires this to the procmgr-side mapping of TokenHandle
    // → ViewObjectId, set up in Task 9's adapter when a token is minted.
    let _ = (token, caller_pid);
    Err(SpawnError::ViewDeriveDenied) // placeholder; real impl uses a side table
}

fn load_manifest_from_vfs(image: &str) -> Option<CachedManifest> {
    // Calls the existing manifest-reading helper. The engineer wires this
    // to the function identified in Task 6 Step 1.
    let _ = image;
    None // placeholder; replace with real loader
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[derive(Clone, Default)]
struct RollbackList {
    view: Option<ViewObjectId>,
    session_id: Option<u32>,
    notify_token: Option<TokenHandle>,
    space: Option<u64>,
    installed_fds: Vec<u32>,
    process_entry_pid: Option<u32>,
}

fn rollback_all(rb: RollbackList) {
    if let Some(v) = rb.view {
        VIEW_TABLE.dec_ref(v);
    }
    if let Some(sid) = rb.session_id {
        hooks::dec_session_refcount(sid);
    }
    if let Some(t) = rb.notify_token {
        hooks::revoke_procmgr_token(t);
    }
    if let Some(space) = rb.space {
        hooks::destroy_space(space);
    }
    // installed_fds, process_entry_pid: clean-up via existing helpers
    // when the engineer wires them.
    let _ = rb.installed_fds;
    let _ = rb.process_entry_pid;
}
```

- [ ] **Step 3: Add module decl**

In `userspace/procmgr/src/lib.rs`:

```rust
pub mod spawn;
```

- [ ] **Step 4: Wire the `hooks::*` placeholders to existing procmgr helpers**

This is the longest sub-step. The engineer:

1. Reads the current `handle_spawn_message` and `handle_container_run` in `userspace/procmgr/src/main.rs` (lines around 4419 and 5482 respectively — verify with `grep -n`).
2. Identifies the helper used at each integration point.
3. Replaces each `hooks::*` body with a `pub(crate) use crate::main::<existing_helper>` or extracts the helper into a `pub(crate)` function inside `main.rs`, then imports it into `spawn.rs`.

For example, the existing `resolve_notify_endpoint` call in `main.rs` becomes the body of `hooks::resolve_token` + `hooks::derive_send` (split the existing one-shot resolve+derive into the two operations the new code calls).

After all hooks are wired, no `unimplemented!()` remains in `spawn.rs`. Run:

```
grep -n "unimplemented" userspace/procmgr/src/spawn.rs
```

Expected: 0 hits.

- [ ] **Step 5: Build**

Run: `cd /home/vlb2bp/git/cluu && cargo build -p procmgr`

Expected: clean build. Compilation errors at this step are normal if some hooks lack matching helpers; resolve each by either inlining the existing main.rs logic or marking the helper `pub(crate)` so spawn.rs can call it.

- [ ] **Step 6: Commit**

```bash
git add userspace/procmgr/src/lib.rs userspace/procmgr/src/spawn.rs
git commit -m "feat(procmgr): procmgr::spawn() core function"
```

---

## Task 9: Wire `PROCMGR_SPAWN_UNIFIED_LABEL = 80` IPC dispatch

**Goal:** A new dispatch arm in procmgr's IPC handler that deserializes a `SpawnEnvelope`, calls `procmgr::spawn`, and serializes the reply. Old `PROCMGR_SPAWN_LABEL` and `PROCMGR_CONTAINER_RUN_LABEL` arms still work; they'll be retired in Task 16.

**Files:**
- Modify: `userspace/procmgr/src/main.rs`

- [ ] **Step 1: Locate the IPC dispatch site**

Run: `cd /home/vlb2bp/git/cluu && grep -n "msg.tag.label == PROCMGR_CONTAINER_RUN_LABEL\|msg.tag.label == PROCMGR_SPAWN_LABEL" userspace/procmgr/src/main.rs`

Note the line numbers. The dispatch is in the main receive loop.

- [ ] **Step 2: Add the unified-spawn arm**

Find the line that matches `PROCMGR_CONTAINER_RUN_LABEL` (around `main.rs:2137`). Immediately before that arm, add:

```rust
if msg.tag.label == cluu_proto::spawn::PROCMGR_SPAWN_UNIFIED_LABEL {
    return self.handle_spawn_unified(msg, payload, sender_tid);
}
```

- [ ] **Step 3: Add the `handle_spawn_unified` method**

Inside the same `impl` block (find another `fn handle_*` method to anchor placement). Add:

```rust
fn handle_spawn_unified(
    &mut self,
    msg: Message,
    payload: &[u8],
    sender_tid: TidLike,
) -> ReplyResult {
    use cluu_proto::spawn::{SpawnEnvelope, SpawnError, SpawnReply};
    use cluu_proto::ABI_VERSION;

    // ABI check (words[1]).
    if msg.tag.words[1] != ABI_VERSION {
        return self.reply_err_spawn_unified(SpawnError::Internal(0xE_BADABI), msg.tag.reply_id);
    }

    // Deserialize.
    let envelope: SpawnEnvelope = match postcard::from_bytes(payload) {
        Ok(e) => e,
        Err(_) => return self.reply_err_spawn_unified(
            SpawnError::Internal(0xE_BADENV),
            msg.tag.reply_id,
        ),
    };

    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);

    // Call the core spawn function.
    let result = crate::spawn::spawn(envelope, caller_pid);

    // Serialize reply.
    let reply_bytes = postcard::to_allocvec(&result).expect("postcard serialize result");
    self.send_reply(msg.tag.reply_id, cluu_proto::spawn::PROCMGR_SPAWN_UNIFIED_LABEL,
                    ABI_VERSION, &reply_bytes)
}

fn reply_err_spawn_unified(
    &mut self,
    err: cluu_proto::spawn::SpawnError,
    reply_id: u64,
) -> ReplyResult {
    use cluu_proto::ABI_VERSION;
    let result: Result<cluu_proto::spawn::SpawnReply, cluu_proto::spawn::SpawnError> = Err(err);
    let reply_bytes = postcard::to_allocvec(&result).expect("postcard serialize err");
    self.send_reply(reply_id, cluu_proto::spawn::PROCMGR_SPAWN_UNIFIED_LABEL,
                    ABI_VERSION, &reply_bytes)
}
```

If `send_reply`, `tid_to_pid`, `TidLike`, `Message`, `ReplyResult` don't match procmgr's actual types, look at the signatures of the existing `handle_container_run` and copy its parameter / return-type pattern. The engineer adapts the function signature to match.

- [ ] **Step 4: Build**

Run: `cd /home/vlb2bp/git/cluu && cargo build -p procmgr`

Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "feat(procmgr): PROCMGR_SPAWN_UNIFIED_LABEL = 80 dispatch"
```

---

## Task 10: libcluu `spawn` public API

**Goal:** `libcluu::spawn(envelope) -> Result<SpawnReply, SpawnError>` — caller surface.

**Files:**
- Modify: `userspace/libcluu/src/ipc.rs` (or a new `userspace/libcluu/src/spawn.rs` per the engineer's call)

- [ ] **Step 1: Find the procmgr-endpoint resolver**

Run: `cd /home/vlb2bp/git/cluu && grep -n "procmgr_endpoint\|PROCMGR_ENDPOINT\|fn call_procmgr" userspace/libcluu/src/ipc.rs | head -10`

Note the existing helper that already issues calls to procmgr (likely `call_procmgr` or similar).

- [ ] **Step 2: Add `libcluu::spawn`**

In `userspace/libcluu/src/ipc.rs`, at the bottom of the file (or in a new submodule):

```rust
/// Issue a `PROCMGR_SPAWN_UNIFIED_LABEL` call to procmgr.
pub fn spawn(
    envelope: cluu_proto::spawn::SpawnEnvelope,
) -> Result<cluu_proto::spawn::SpawnReply, cluu_proto::spawn::SpawnError> {
    use cluu_proto::ABI_VERSION;
    use cluu_proto::spawn::{PROCMGR_SPAWN_UNIFIED_LABEL, SpawnError, SpawnReply};

    let payload = match postcard::to_allocvec(&envelope) {
        Ok(b) => b,
        Err(_) => return Err(SpawnError::Internal(0xE_LOCAL_SER)),
    };

    // Build the message: words[0]=payload_len, words[1]=ABI_VERSION, words[2..6]=0
    let mut words = [0u64; 6];
    words[0] = payload.len() as u64;
    words[1] = ABI_VERSION as u64;

    let reply = match call_procmgr(PROCMGR_SPAWN_UNIFIED_LABEL, words, &payload) {
        Ok(r) => r,
        Err(_) => return Err(SpawnError::Internal(0xE_PROCMGR_DEAD)),
    };

    let result: Result<SpawnReply, SpawnError> = match postcard::from_bytes(&reply.payload) {
        Ok(r) => r,
        Err(_) => return Err(SpawnError::Internal(0xE_LOCAL_DESER)),
    };
    result
}
```

If `call_procmgr` has a different signature (e.g., takes `&[u64; 6]` instead of `[u64; 6]`), adapt accordingly.

- [ ] **Step 3: Build**

Run: `cd /home/vlb2bp/git/cluu && cargo build -p libcluu`

Expected: clean build.

- [ ] **Step 4: Smoke test via probe**

Create a minimal probe binary at `userspace/probes/spawn_unified_smoke/`. First confirm the workspace template (look at an existing simple probe like `userspace/probes/argvprobe/`). Copy its `Cargo.toml` shape; rename `name = "spawn_unified_smoke"`; in its `src/main.rs`:

```rust
#![no_std]
#![no_main]

extern crate alloc;
extern crate libcluu;
use libcluu::print_log;

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    // Issue a self-spawn of the smoke probe with a tiny argv. Procmgr should
    // either succeed (returning Ok with a new pid) or return a concrete
    // SpawnError discriminant. Either way, the test verifies wire round-trip.
    let envelope = cluu_proto::spawn::SpawnEnvelope {
        image: alloc::string::String::from("spawn_unified_smoke_child"),
        args: alloc::vec::Vec::new(),
        env: alloc::vec::Vec::new(),
        view: cluu_proto::spawn::ViewSource::Derive(0), // bogus; procmgr will Err
        fd_inherit: alloc::vec::Vec::new(),
        session: None,
        notify: None,
    };
    match libcluu::ipc::spawn(envelope) {
        Ok(reply) => {
            print_log(&alloc::format!("l3_spawn_unified_smoke: OK pid={}\n", reply.pid));
        }
        Err(e) => {
            print_log(&alloc::format!("l3_spawn_unified_smoke: ERR {:?}\n", e));
        }
    }
    0
}
```

Add the probe to the workspace members in the root `Cargo.toml`.

- [ ] **Step 5: Run smoke test**

```
cd /home/vlb2bp/git/cluu
HARNESS_FORCE_BUILD=1 CLUU_SHELL_AUTOSTART_CMD=spawn_unified_smoke MARKER_MODE=l3_spawn_unified_smoke bash scripts/harness_run.sh
grep "l3_spawn_unified_smoke:" serial.log
```

Expected output (one of):
- `l3_spawn_unified_smoke: OK pid=<num>` if the deliberately-bogus token happens to resolve (unlikely).
- `l3_spawn_unified_smoke: ERR ImageNotFound` — most likely; proves wire round-trip works.
- `l3_spawn_unified_smoke: ERR ViewDeriveDenied` — also acceptable; proves rounded trip.

A `Internal(0xE_PROCMGR_DEAD)` or no marker line at all = failure. Diagnose by checking serial log around the call.

- [ ] **Step 6: Commit**

```bash
git add userspace/libcluu/src/ipc.rs userspace/probes/spawn_unified_smoke/ Cargo.toml
git commit -m "feat(libcluu): spawn() wrapper + smoke probe"
```

---

## Task 11: Reduce kernel `launch_service` to `launch_procmgr`

**Goal:** Init's kernel-spawn path collapses from "primordial batch loop" to just "spawn procmgr". All other primordials wait for PRIMORDIAL_SEED.

**Files:**
- Modify: `userspace/init/src/wiring.rs`
- Modify: `kernel/src/<file>` (located by grep)

- [ ] **Step 1: Locate kernel-side launch_service**

Run: `cd /home/vlb2bp/git/cluu && grep -rn "fn launch_service\|launch_service(" kernel/src/ userspace/init/src/ 2>/dev/null | head -10`

Identify the kernel-side function and any user-side syscall wrapper.

- [ ] **Step 2: Read init's current primordial-spawn loop**

Run: `cd /home/vlb2bp/git/cluu && grep -n "launch_service\|primordial" userspace/init/src/wiring.rs | head -20`

The loop probably iterates over a static list of primordial entries (registry, timeserver, procmgr, vfs, virtio-blk, tpmd) and calls `launch_service` for each. Note the iteration site.

- [ ] **Step 3: Preserve the existing loop behind a feature flag for now**

To stay incremental and harness-green, don't delete yet. Add a guard around the *non-procmgr* primordials so init still launches them via the legacy kernel-spawn path UNTIL Task 12 lands `PROCMGR_PRIMORDIAL_SEED` handling on the procmgr side. In `userspace/init/src/wiring.rs`, near the primordial loop, restructure:

```rust
// Step 1: kernel-spawn procmgr alone.
launch_service("procmgr", /* args */);

// Step 2 (LANDING IN TASK 13): the rest go via PROCMGR_PRIMORDIAL_SEED.
// Until Task 13 wires the seed, fall back to legacy launch_service for them:
for primordial in OTHER_PRIMORDIALS {
    launch_service(primordial.image, primordial.args);
}
```

`OTHER_PRIMORDIALS` is the existing primordial list minus procmgr. The engineer extracts the procmgr entry from the existing list.

- [ ] **Step 4: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
bash scripts/harness_run.sh
```

Expected: boot reaches `compositor: ready` as before. No regression.

- [ ] **Step 5: Commit**

```bash
git add userspace/init/src/wiring.rs
git commit -m "refactor(init): separate procmgr kernel-spawn from other primordials"
```

---

## Task 12: PROCMGR_PRIMORDIAL_SEED handler

**Goal:** Procmgr accepts a one-shot `PROCMGR_PRIMORDIAL_SEED_LABEL = 81` call from init. Spawns each envelope sequentially via `procmgr::spawn` with the `BootstrapRoot` view path enabled.

**Files:**
- Modify: `userspace/procmgr/src/main.rs`
- Modify: `userspace/procmgr/src/spawn.rs` (add a `seed_in_progress` flag)

- [ ] **Step 1: Add a global flag for "primordial seed handler active"**

In `userspace/procmgr/src/spawn.rs`, near the top:

```rust
use core::sync::atomic::{AtomicBool, Ordering};

/// True while the primordial-seed handler is running. Gates `ViewSource::BootstrapRoot`.
pub static SEED_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
pub static SEED_CONSUMED: AtomicBool = AtomicBool::new(false);
```

In the `BootstrapRoot` branch inside `spawn()`, replace the `caller_pid != hooks::init_pid()` check with:

```rust
if caller_pid != hooks::init_pid() || !SEED_IN_PROGRESS.load(Ordering::Acquire) {
    return Err(SpawnError::ViewDeriveDenied);
}
```

- [ ] **Step 2: Add the SEED dispatch arm**

In `userspace/procmgr/src/main.rs`, in the IPC dispatch loop (same place as Task 9), add:

```rust
if msg.tag.label == cluu_proto::primordial::PROCMGR_PRIMORDIAL_SEED_LABEL {
    return self.handle_primordial_seed(msg, payload, sender_tid);
}
```

- [ ] **Step 3: Add the handler method**

```rust
fn handle_primordial_seed(
    &mut self,
    msg: Message,
    payload: &[u8],
    sender_tid: TidLike,
) -> ReplyResult {
    use core::sync::atomic::Ordering;
    use cluu_proto::primordial::{PrimordialSeed, PrimordialSeedReply,
                                  PROCMGR_PRIMORDIAL_SEED_LABEL};
    use cluu_proto::spawn::SpawnError;
    use cluu_proto::ABI_VERSION;

    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);

    if msg.tag.words[1] != ABI_VERSION {
        return self.send_reply(msg.tag.reply_id, PROCMGR_PRIMORDIAL_SEED_LABEL, ABI_VERSION,
            &postcard::to_allocvec(&PrimordialSeedReply { results: alloc::vec::Vec::new() })
                .expect("ser"));
    }

    if caller_pid != crate::spawn::hooks::init_pid()
        || crate::spawn::SEED_CONSUMED.load(Ordering::Acquire)
    {
        let reply = PrimordialSeedReply {
            results: alloc::vec![Err(SpawnError::PermissionDenied)],
        };
        return self.send_reply(msg.tag.reply_id, PROCMGR_PRIMORDIAL_SEED_LABEL, ABI_VERSION,
            &postcard::to_allocvec(&reply).expect("ser"));
    }

    let seed: PrimordialSeed = match postcard::from_bytes(payload) {
        Ok(s) => s,
        Err(_) => {
            let reply = PrimordialSeedReply {
                results: alloc::vec![Err(SpawnError::Internal(0xE_BADENV))],
            };
            return self.send_reply(msg.tag.reply_id, PROCMGR_PRIMORDIAL_SEED_LABEL, ABI_VERSION,
                &postcard::to_allocvec(&reply).expect("ser"));
        }
    };

    crate::spawn::SEED_IN_PROGRESS.store(true, Ordering::Release);
    let mut results = alloc::vec::Vec::with_capacity(seed.primordials.len());
    for env in seed.primordials {
        results.push(crate::spawn::spawn(env, caller_pid));
    }
    crate::spawn::SEED_IN_PROGRESS.store(false, Ordering::Release);
    crate::spawn::SEED_CONSUMED.store(true, Ordering::Release);

    let reply = PrimordialSeedReply { results };
    self.send_reply(msg.tag.reply_id, PROCMGR_PRIMORDIAL_SEED_LABEL, ABI_VERSION,
        &postcard::to_allocvec(&reply).expect("ser"))
}
```

The engineer adjusts type names (`Message`, `TidLike`, `ReplyResult`) to match procmgr's actual signatures.

- [ ] **Step 4: Build**

Run: `cd /home/vlb2bp/git/cluu && cargo build -p procmgr`

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs userspace/procmgr/src/spawn.rs
git commit -m "feat(procmgr): PROCMGR_PRIMORDIAL_SEED handler (label 81)"
```

---

## Task 13: Init sends PRIMORDIAL_SEED for non-procmgr primordials

**Goal:** Init's primordial loop replaced by one PRIMORDIAL_SEED IPC.

**Files:**
- Modify: `userspace/init/src/wiring.rs`

- [ ] **Step 1: Add libcluu surface for PRIMORDIAL_SEED**

In `userspace/libcluu/src/ipc.rs` (near the new `spawn` function from Task 10), add:

```rust
pub fn primordial_seed(
    seed: cluu_proto::primordial::PrimordialSeed,
) -> Result<cluu_proto::primordial::PrimordialSeedReply, ()> {
    use cluu_proto::ABI_VERSION;
    use cluu_proto::primordial::PROCMGR_PRIMORDIAL_SEED_LABEL;

    let payload = postcard::to_allocvec(&seed).map_err(|_| ())?;
    let mut words = [0u64; 6];
    words[0] = payload.len() as u64;
    words[1] = ABI_VERSION as u64;
    let reply = call_procmgr(PROCMGR_PRIMORDIAL_SEED_LABEL, words, &payload).map_err(|_| ())?;
    postcard::from_bytes(&reply.payload).map_err(|_| ())
}
```

- [ ] **Step 2: Rewrite init's primordial loop**

In `userspace/init/src/wiring.rs`, replace the `for primordial in OTHER_PRIMORDIALS { launch_service(...) }` block with a `PrimordialSeed` construction:

```rust
use cluu_proto::primordial::PrimordialSeed;
use cluu_proto::spawn::{SpawnEnvelope, ViewSource};

// Build envelopes for each primordial. Each uses ViewSource::BootstrapRoot
// (procmgr accepts this only during the SEED handler).
let primordials = alloc::vec![
    build_envelope("registry"),
    build_envelope("timeserver"),
    build_envelope("vfs"),
    build_envelope("virtio-blk"),
    build_envelope("tpmd"),
];

let seed = PrimordialSeed { primordials };
let reply = libcluu::ipc::primordial_seed(seed)
    .expect("primordial_seed IPC must succeed");

// Panic on any failed primordial — init's monitor expects all to come up.
for (idx, result) in reply.results.iter().enumerate() {
    if let Err(e) = result {
        panic!("primordial {} failed to spawn: {:?}", idx, e);
    }
}
```

Add a helper `build_envelope`:

```rust
fn build_envelope(image: &str) -> SpawnEnvelope {
    SpawnEnvelope {
        image: alloc::string::String::from(image),
        args: alloc::vec::Vec::new(),
        env: alloc::vec::Vec::new(),
        view: ViewSource::BootstrapRoot,
        fd_inherit: alloc::vec::Vec::new(),
        session: None,
        notify: Some(init_exit_endpoint_token()),  // see hook below
    }
}

fn init_exit_endpoint_token() -> cluu_proto::TokenHandle {
    // Init already creates an exit endpoint for primordial monitoring
    // (MEMORY.md §15). Wire to the existing helper.
    unimplemented!("wire to init's exit-endpoint accessor")
}
```

The engineer resolves `init_exit_endpoint_token` to the existing init code.

- [ ] **Step 3: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
bash scripts/harness_run.sh
```

Expected: boot reaches `compositor: ready`. All primordials come up via the new SEED path.

If a primordial fails to come up, check serial log for `primordial N failed to spawn: ...`. The error discriminant points at which procmgr step rejected the envelope.

- [ ] **Step 4: Commit**

```bash
git add userspace/init/src/wiring.rs userspace/libcluu/src/ipc.rs
git commit -m "feat(init): spawn primordials via PROCMGR_PRIMORDIAL_SEED"
```

---

## Task 14: Procmgr autostart uses `procmgr::spawn`

**Goal:** Autostart-loop replaces the existing `autostart_container()` body (around `userspace/procmgr/src/main.rs:1303`) with a `procmgr::spawn` in-process call.

**Files:**
- Modify: `userspace/procmgr/src/main.rs`

- [ ] **Step 1: Locate the existing autostart code**

Run: `cd /home/vlb2bp/git/cluu && grep -n "autostart_container\|autostart.toml\|AUTOSTART" userspace/procmgr/src/main.rs | head -10`

Note the function and the parser that loads `/etc/autostart.toml`.

- [ ] **Step 2: Rewrite the autostart loop**

Replace `autostart_container(...)` body with a `SpawnEnvelope` construction + `crate::spawn::spawn(envelope, procmgr_self_pid)`:

```rust
fn autostart_container(&mut self, entry: &AutostartEntry) -> Result<u32, ()> {
    use cluu_proto::spawn::{SpawnEnvelope, ViewSource};

    let envelope = SpawnEnvelope {
        image: entry.image.clone(),
        args: entry.args.clone(),
        env: alloc::vec::Vec::new(),
        view: ViewSource::Derive(self.system_view_token()), // procmgr's system view
        fd_inherit: alloc::vec::Vec::new(),
        session: None,  // autostart system services are sessionless
        notify: None,
    };

    match crate::spawn::spawn(envelope, self.procmgr_self_pid()) {
        Ok(reply) => Ok(reply.pid),
        Err(e) => {
            log::error!("autostart of {} failed: {:?}", entry.image, e);
            Err(())
        }
    }
}
```

The engineer wires `system_view_token()` (procmgr's system view, established at boot) and `procmgr_self_pid()`. If `entry.restart_policy` exists in the existing autostart.toml schema, REMOVE it — the spawned image's manifest is the source of truth (spec 1 §11). Delete the parser field.

- [ ] **Step 3: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
bash scripts/harness_run.sh
```

Expected: boot reaches `compositor: ready` (autostart spawns compositor via the new path).

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/main.rs /home/vlb2bp/git/cluu/etc/autostart.toml
git commit -m "refactor(procmgr): autostart uses procmgr::spawn directly"
```

If autostart.toml didn't change, omit it from `git add`.

---

## Task 15: SESSION_LOGIN internal spawns use `procmgr::spawn`

**Goal:** Inside `handle_session_login` (around `userspace/procmgr/src/main.rs:2143`), the calls that spawn compositor and cluuterm flip to `crate::spawn::spawn`. The 2 s `COMPOSITOR_READY` wait stays for now (spec 3's job to delete).

**Files:**
- Modify: `userspace/procmgr/src/main.rs`

- [ ] **Step 1: Read the existing function**

Run: `cd /home/vlb2bp/git/cluu && grep -n "fn handle_session_login\|spawn_user_compositor\|spawn_cluuterm" userspace/procmgr/src/main.rs | head -10`

Note the function structure: it kills the system compositor, spawns the user compositor, waits, spawns cluuterm.

- [ ] **Step 2: Replace internal spawn calls**

Replace the two internal spawn helpers (`spawn_user_compositor`, `spawn_cluuterm`) with `crate::spawn::spawn(envelope, procmgr_self_pid)` calls building envelopes from the session's profile + the current spawn parameters.

Example, the cluuterm spawn:

```rust
fn spawn_cluuterm(&mut self, session: &SessionEntry) -> Result<u32, ()> {
    use cluu_proto::spawn::{SpawnEnvelope, ViewSource};

    let envelope = SpawnEnvelope {
        image: alloc::string::String::from("cluuterm"),
        args: alloc::vec::Vec::new(),
        env: session.env_inline.clone(),
        view: ViewSource::Derive(session.view_token),
        fd_inherit: alloc::vec::Vec::new(), // cluuterm has no inherited fds
        session: Some(session.session_token), // spec 3 territory; placeholder until spec 3 lands
        notify: Some(self.session_login_notify_token()),
    };

    crate::spawn::spawn(envelope, self.procmgr_self_pid())
        .map(|r| r.pid)
        .map_err(|e| { log::error!("cluuterm spawn failed: {:?}", e); () })
}
```

If `session_token` doesn't exist yet (spec 3 hasn't landed), pass `None` and accept that session integration is a spec 3 step.

- [ ] **Step 3: Build + boot smoke + manual login**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
bash scripts/harness_run.sh
```

Test interactive root/root login if the harness supports it. Verify shell prompt visible.

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "refactor(procmgr): SESSION_LOGIN internal spawns use procmgr::spawn"
```

---

## Task 16: libcluu newlib `posix_spawn` shim translates to `SpawnEnvelope`

**Goal:** Newlib's `posix_spawn` POSIX surface stays for ported C programs (MicroPython subprocess), but underneath it builds a `SpawnEnvelope` and calls `libcluu::ipc::spawn`. `posix_spawn_file_actions_adddup2` translates to `FdInherit` entries parent-side.

**Files:**
- Modify: `userspace/libcluu/src/posix/process.rs` (locate the existing `posix_spawn`)

- [ ] **Step 1: Locate the existing shim**

Run: `cd /home/vlb2bp/git/cluu && grep -n "posix_spawn\|posix_spawn_file_actions" userspace/libcluu/src/posix/process.rs | head -20`

Read the current implementation. It currently builds a `PROCMGR_SPAWN_LABEL` payload.

- [ ] **Step 2: Translate to `SpawnEnvelope`**

Refactor the shim:

1. Walk `file_actions` (the existing `posix_spawn_file_actions_t`).
2. For each `adddup2(src_fd, dst_fd)`: convert `src_fd` (parent-side) to `(vfs_client_id, vfs_remote_fd)` via the existing libcluu `fd_table::vfs_addr(src_fd)` helper. Append `FdInherit { child_fd: dst_fd, source: VfsFd { vfs_client_id, vfs_remote_fd }, rights: FdRights::READ_WRITE }`.
3. For each `addopen(path, flags, mode, target_fd)`: open the path in the caller's process, take its resulting fd's vfs address, append `FdInherit` entry, schedule a `close(parent_fd)` after the spawn call.
4. For each `addclose(fd)`: this is a hint that `fd` should NOT be inherited; ensure no `FdInherit` entry references it (default behavior — fds are only inherited if explicitly listed).
5. Build `SpawnEnvelope` with `image` resolved from `path` (use the existing `path_to_image_name` helper if present; if not, scan `/var/images/*/manifest.toml` for matching `ENTRYPOINT`).
6. Call `libcluu::ipc::spawn(envelope)`.
7. Map `SpawnReply.pid` to POSIX `pid_t` return; `SpawnError` to errno.

The translation logic is ~80 lines. The existing shim is a useful skeleton.

- [ ] **Step 3: Build + run MicroPython subprocess test (if available in tree)**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_mp_etc bash scripts/harness_run.sh
grep "l2_mp_etc:" serial.log
```

Expected: GREEN (existing marker for MicroPython behavior).

- [ ] **Step 4: Commit**

```bash
git add userspace/libcluu/src/posix/process.rs
git commit -m "refactor(libcluu): posix_spawn shim translates to SpawnEnvelope"
```

---

## Task 17: Cluuterm flips to `libcluu::spawn` direct (retires dup2)

**Goal:** Cluuterm's shell-spawn code at `userspace/cluuterm/src/main.rs:241` (`spawn_shell_with_pts`) calls `libcluu::ipc::spawn` directly. No newlib `posix_spawn`, no `adddup2`.

**Files:**
- Modify: `userspace/cluuterm/src/main.rs`

- [ ] **Step 1: Locate the spawn site**

Run: `cd /home/vlb2bp/git/cluu && grep -n "spawn_shell_with_pts\|posix_spawn_file_actions_adddup2\|posix_spawn(" userspace/cluuterm/src/main.rs | head -10`

Note the function and the surrounding code that opens `/dev/pts/<id>`.

- [ ] **Step 2: Replace with direct spawn**

Rewrite the spawn site:

```rust
fn spawn_shell_with_pts(&mut self, pts_id: u32) -> Result<u32, ()> {
    use cluu_proto::spawn::{FdInherit, FdRights, FdSource, SpawnEnvelope, ViewSource};

    // Open /dev/pts/<n> three times for stdin/stdout/stderr.
    let path = alloc::format!("/dev/pts/{}", pts_id);
    let pts_fd_stdin  = libcluu::posix::open(&path, libcluu::posix::O_RDONLY).map_err(|_| ())?;
    let pts_fd_stdout = libcluu::posix::open(&path, libcluu::posix::O_WRONLY).map_err(|_| ())?;
    let pts_fd_stderr = libcluu::posix::open(&path, libcluu::posix::O_WRONLY).map_err(|_| ())?;

    let (stdin_cid, stdin_rfd)   = libcluu::fd_table::vfs_addr(pts_fd_stdin).ok_or(())?;
    let (stdout_cid, stdout_rfd) = libcluu::fd_table::vfs_addr(pts_fd_stdout).ok_or(())?;
    let (stderr_cid, stderr_rfd) = libcluu::fd_table::vfs_addr(pts_fd_stderr).ok_or(())?;

    let envelope = SpawnEnvelope {
        image: alloc::string::String::from("shell"),
        args: alloc::vec::Vec::new(),
        env: alloc::vec![
            (alloc::string::String::from("TERM"), alloc::string::String::from("xterm-256color")),
            (alloc::string::String::from("HOME"), self.user_home()),
            (alloc::string::String::from("USER"), self.user_name()),
        ],
        view: ViewSource::Derive(self.cluuterm_view_token()),
        fd_inherit: alloc::vec![
            FdInherit { child_fd: 0, source: FdSource::VfsFd { vfs_client_id: stdin_cid,  vfs_remote_fd: stdin_rfd  }, rights: FdRights::READ_ONLY },
            FdInherit { child_fd: 1, source: FdSource::VfsFd { vfs_client_id: stdout_cid, vfs_remote_fd: stdout_rfd }, rights: FdRights::WRITE_ONLY },
            FdInherit { child_fd: 2, source: FdSource::VfsFd { vfs_client_id: stderr_cid, vfs_remote_fd: stderr_rfd }, rights: FdRights::WRITE_ONLY },
        ],
        session: self.session_token().map(|t| t),  // None if pre-spec-3
        notify: None,
    };

    let reply = libcluu::ipc::spawn(envelope).map_err(|e| {
        libcluu::print_log(&alloc::format!("cluuterm: spawn failed {:?}\n", e));
        ()
    })?;

    // Close parent-side fds; child holds its own derived caps now.
    libcluu::posix::close(pts_fd_stdin);
    libcluu::posix::close(pts_fd_stdout);
    libcluu::posix::close(pts_fd_stderr);

    Ok(reply.pid)
}
```

Remove any `posix_spawn_file_actions_t` related code from `spawn_shell_with_pts` and its callers.

- [ ] **Step 3: Build + boot smoke + interactive login**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
bash scripts/harness_run.sh
```

Test interactive login. Verify shell prompt + typed input echo.

- [ ] **Step 4: Verify "two cluuterms" bug is fixed (manual)**

In the running shell, run `ps` (or query procmgr for ProcessEntry.comm via the debug log). The cluuterm-spawned shell's `comm` field should be `"shell"`, not `"cluuterm"`.

- [ ] **Step 5: Commit**

```bash
git add userspace/cluuterm/src/main.rs
git commit -m "refactor(cluuterm): spawn shell via libcluu::spawn directly (retire dup2)"
```

---

## Task 18: Restart-policy moves from autostart.toml to manifests

**Goal:** Drop any `restart_policy` column from `autostart.toml` parser; ensure each image's manifest declares `RESTART` directive. Procmgr stores `child.restart_policy = manifest.restart_policy` (already wired in `procmgr::insert_process_entry` from Task 8).

**Files:**
- Modify: `userspace/procmgr/src/main.rs` (autostart parser)
- Modify: `/etc/autostart.toml` and various `/var/images/<image>/manifest.toml` files
- Modify: Cluufile parser if needed

- [ ] **Step 1: Find the autostart parser**

Run: `cd /home/vlb2bp/git/cluu && grep -n "fn parse_autostart\|restart_policy" userspace/procmgr/src/main.rs | head -10`

If the autostart parser has a `restart_policy` field per entry, remove it.

- [ ] **Step 2: Add RESTART directive to manifest parser**

Find the Cluufile/manifest parser (probably in `userspace/procmgr/src/envelopes.rs` or `userspace/init/src/...`). Add parsing for:

```
RESTART never
RESTART always
RESTART on_failure max=5 window=60000
```

Map to the `RestartPolicy` enum in `cluu_proto::spawn`.

- [ ] **Step 3: Update each /var/images/<image>/manifest.toml that needs RESTART**

Compositor manifest needs `RESTART always`. Login manifest stays `RESTART never`. Primordials all `RESTART never`. Update relevant files.

- [ ] **Step 4: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
bash scripts/harness_run.sh
```

Expected: clean boot. Test that crashing the compositor (e.g., manually `kill -9 <pid>`) triggers respawn per the manifest.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs userspace/procmgr/src/envelopes.rs /home/vlb2bp/git/cluu/etc/autostart.toml /home/vlb2bp/git/cluu/var/images/
git commit -m "feat(procmgr): manifest declares RESTART policy (envelope no longer carries it)"
```

---

## Task 19: Acceptance markers

**Goal:** Add the new harness markers from spec 1 §15. Each marker is a small probe binary that exercises a specific spec-required behavior.

**Files:**
- Create: `userspace/probes/l2_spawn_view_widen_denied/`
- Create: `userspace/probes/l2_spawn_fd_inherit_widen_denied/`
- Create: `userspace/probes/l2_primordial_seed_caller_check/`
- Create: `userspace/probes/l2_spawn_identity_basename/`

For each marker, follow this template (using `l2_spawn_view_widen_denied` as an example):

- [ ] **Step 1: Scaffold a probe**

Copy `userspace/probes/argvprobe/Cargo.toml` to `userspace/probes/l2_spawn_view_widen_denied/Cargo.toml`. Rename the `[package] name`.

Add `"userspace/probes/l2_spawn_view_widen_denied",` to the workspace `Cargo.toml`.

- [ ] **Step 2: Write the probe `src/main.rs`**

```rust
#![no_std]
#![no_main]
extern crate alloc;
extern crate libcluu;

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    use cluu_proto::spawn::{SpawnEnvelope, SpawnError, ViewSource};

    // Build an envelope whose view derive would widen.
    // (The exact construction depends on whether you can deliberately ask
    // for a "widening" view. The probe asserts the error discriminant.)
    let env = SpawnEnvelope {
        image: alloc::string::String::from("shell"),
        args: alloc::vec::Vec::new(),
        env: alloc::vec::Vec::new(),
        view: ViewSource::Derive(0), // bogus token = ViewDeriveDenied path
        fd_inherit: alloc::vec::Vec::new(),
        session: None,
        notify: None,
    };

    let result = libcluu::ipc::spawn(env);
    match result {
        Err(SpawnError::ViewDeriveDenied) => {
            libcluu::print_log(b"l2_spawn_view_widen_denied: PASS\n");
        }
        other => {
            libcluu::print_log(&alloc::format!(
                "l2_spawn_view_widen_denied: FAIL unexpected {:?}\n", other));
        }
    }
    0
}
```

Adjust per the specific marker (`l2_spawn_fd_inherit_widen_denied` exercises the FdInherit rights check; `l2_primordial_seed_caller_check` issues `PROCMGR_PRIMORDIAL_SEED` from a non-init binary and expects PermissionDenied; `l2_spawn_identity_basename` reads the process's own `argv[0]` post-spawn and verifies it equals `basename(manifest.entrypoint)`).

- [ ] **Step 3: Run each marker**

For each `<marker>`:

```
cd /home/vlb2bp/git/cluu
HARNESS_FORCE_BUILD=1 CLUU_SHELL_AUTOSTART_CMD=<marker> MARKER_MODE=<marker> bash scripts/harness_run.sh
grep "<marker>: " serial.log
```

Expected: `<marker>: PASS` for each.

- [ ] **Step 4: Commit**

```bash
git add userspace/probes/l2_spawn_view_widen_denied/ \
         userspace/probes/l2_spawn_fd_inherit_widen_denied/ \
         userspace/probes/l2_primordial_seed_caller_check/ \
         userspace/probes/l2_spawn_identity_basename/ \
         Cargo.toml
git commit -m "test: spec 1 acceptance markers"
```

---

## Task 20: Delete dead code

**Goal:** Remove the now-unreachable legacy spawn paths once everything routes through the unified verb.

**Files:**
- Modify: `userspace/procmgr/src/main.rs`
- Modify: `userspace/libcluu/src/ipc.rs`
- Modify: `userspace/init/src/wiring.rs`

- [ ] **Step 1: Find every reference to the legacy labels**

Run:

```
cd /home/vlb2bp/git/cluu
git grep -n "PROCMGR_SPAWN_LABEL\b" || true
git grep -n "PROCMGR_CONTAINER_RUN_LABEL\b" || true
git grep -n "handle_spawn_message\b" || true
git grep -n "handle_container_run\b" || true
git grep -n "build_container_run_payload" || true
git grep -n "build_spawn_payload" || true
```

For each hit, the engineer either:
- Deletes the code if it's no longer reachable (the IPC dispatch arms, the handlers themselves, the payload builders).
- Replaces remaining callers with `libcluu::ipc::spawn` if any survived (should be none after Tasks 16-17).

- [ ] **Step 2: Delete the label constants and helpers**

In `userspace/libcluu/src/ipc.rs`, find the `PROCMGR_SPAWN_LABEL` and `PROCMGR_CONTAINER_RUN_LABEL` consts. Delete them.

In `userspace/procmgr/src/main.rs`, find the dispatch arms for both labels. Delete them. Find and delete `fn handle_spawn_message`, `fn handle_container_run`, `fn build_container_run_payload_full`, and `fn build_spawn_payload` (or whatever names exist).

- [ ] **Step 3: Reduce kernel `launch_service` to `launch_procmgr`**

In the kernel file located in Task 11 Step 1, rename `launch_service` to `launch_procmgr` and remove any code path that takes an image-name argument (it only ever spawns procmgr now). If init's `wiring.rs` calls `launch_service(name, args)`, simplify to `launch_procmgr()`.

- [ ] **Step 4: Verify zero hits**

```
cd /home/vlb2bp/git/cluu
git grep -n "PROCMGR_SPAWN_LABEL\b" && echo "FAIL" || echo "PASS"
git grep -n "PROCMGR_CONTAINER_RUN_LABEL\b" && echo "FAIL" || echo "PASS"
git grep -n "handle_spawn_message" && echo "FAIL" || echo "PASS"
git grep -n "handle_container_run" && echo "FAIL" || echo "PASS"
git grep -n "build_container_run_payload" && echo "FAIL" || echo "PASS"
git grep -n "build_spawn_payload" && echo "FAIL" || echo "PASS"
git grep -n "posix_spawn_file_actions_adddup2" userspace/cluuterm/ && echo "FAIL" || echo "PASS"
```

All seven must print `PASS`.

- [ ] **Step 5: Build clean**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: both clean.

- [ ] **Step 6: Final boot smoke**

```
bash scripts/harness_run.sh
```

Expected: `compositor: ready`. Interactive login works. ps shows correct process names.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: delete legacy spawn paths (PROCMGR_SPAWN_LABEL, PROCMGR_CONTAINER_RUN_LABEL)"
```

---

## Final verification

- [ ] **All spec 1 §15 grep proofs:**

```
cd /home/vlb2bp/git/cluu
echo "Zero-hit proofs:"
git grep -c "PROCMGR_SPAWN_LABEL"            # → 0
git grep -c "PROCMGR_CONTAINER_RUN_LABEL"    # → 0
git grep -c "handle_spawn_message"           # → 0
git grep -c "handle_container_run"           # → 0
git grep -c "build_container_run_payload"    # → 0
git grep -c "build_spawn_payload"            # → 0
git grep -c "posix_spawn_file_actions_adddup2" userspace/cluuterm/  # → 0

echo "One-match proofs:"
git grep -c "PROCMGR_SPAWN_UNIFIED_LABEL.*= 80"        # → 1
git grep -c "PROCMGR_PRIMORDIAL_SEED_LABEL.*= 81"      # → 1
git grep -n "fn spawn(.*SpawnEnvelope" userspace/procmgr/src/spawn.rs  # → 1
```

All must match.

- [ ] **Performance baseline:**

```
cd /home/vlb2bp/git/cluu
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_jobchurn_heavy bash scripts/harness_run.sh
grep "l2_jobchurn_heavy:" serial.log
```

Result within 20% of pre-spec-1 baseline (per `feedback_spawn_perf_baseline`).

- [ ] **No new timeouts:**

```
cd /home/vlb2bp/git/cluu
grep -rn "recv_with_timeout\|call_with_timeout" userspace/procmgr/src/ | wc -l
```

Number must equal the pre-spec-1 count (memorize before starting; spec 1 does not add timeouts).

---

## Notes for the engineer

- **TDD pattern:** every task has tests where applicable. Run them before moving to the next task.
- **Frequent commits:** each task ends with a commit. Don't squash; keep the history granular so reverting any single task is trivial.
- **Tool unfamiliarity:** if `cargo xtask build` fails with an opaque error, consult `xtask/src/main.rs` for the build orchestration. Common failure: missing `target/sysroot/x86_64-cluu-elf` — fix with `cargo xtask build-newlib` first.
- **DRY:** the same `procmgr::spawn` function handles autostart, primordial seed, SESSION_LOGIN, and the unified IPC verb. Don't duplicate logic into per-caller helpers.
- **YAGNI:** spec 1 explicitly defers session lifecycle (spec 3) and the 2 s `COMPOSITOR_READY` deletion. Don't touch those here.
- **Test the failure modes:** every `SpawnError` discriminant should be reachable via at least one acceptance marker. If a discriminant is unreachable, either delete it from `SpawnError` or add a marker.
- **If a hook in Task 8 has no existing helper:** either inline the logic from `handle_container_run`/`handle_spawn_message`, or extract the helper into a `pub(crate)` function inside `main.rs` and import it into `spawn.rs`. Both are fine; pick the one with smaller diff per call site.
- **Roll-back discipline:** every successful step in `procmgr::spawn` adds to the `RollbackList`; every later-failure path calls `rollback_all`. Don't shortcut this — partial-state leaks are the worst class of bug spec 1 closes.

---

## Spec 1 sections covered

| Spec § | Task(s) |
|---|---|
| §3 architecture | Task 8, 9, 12 |
| §4 types | Task 2, 3 |
| §5 wire format | Task 2, 3, 9, 10, 12, 13 |
| §6 process identity | Task 8 (basename + argv[0] override) |
| §7 FdInherit | Task 8 (install_fd_inherit), 16 (newlib shim), 17 (cluuterm) |
| §8 view derive | Task 7 (table + monotone), 8 (resolve + bootstrap) |
| §9 session field | Task 8 (resolve_session_token hook) |
| §10 notify field | Task 8 (derive_send hook) |
| §11 restart policy | Task 18 (manifest-driven, not envelope) |
| §12 error semantics + rollback | Task 8 (RollbackList) |
| §13 primordial bootstrap | Task 11, 12, 13 |
| §14 migration plan | Task 1-20 (matches step ordering) |
| §15 acceptance | Task 19, final verification |
| §16 follow-ups | OUT of plan 1 scope |
