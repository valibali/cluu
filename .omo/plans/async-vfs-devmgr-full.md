# Async VFS + devmgr + Kernel Block-Region Caps — Full Plan

**Date:** 2026-07-06
**Branch:** `develop`
**Scope:** Components #1–#4 from the async-where-it-helps list

## Destination

A CLUU where:
1. `/proc` queries never deadlock (procfs → procmgr async, already ~80% done)
2. `/dev/*` driver IPC never deadlocks (tty read + PTS verbs converted to async)
3. A `devmgr` userspace service owns block devices and grants block-region caps, backed by a new kernel `BlockRegion` capability type
4. The VFS async runtime is the canonical dispatch path for ALL VfsOps on async mounts, not just Open/Readdir
5. AGENTS.md §7 is updated to bless the async runtime

## Constraints

- `no_std` + `alloc` (kernel and userspace)
- No `as any`/`unwrap`/`panic` in new code (rust-best-practices skill)
- Kernel near freeze — new cap type must be minimal and clean
- No runtime ACL (§3) — block-region caps are capability-scoped, not policy-checked
- Root session godmode stays root-bound (§6)
- Single-threaded servers — async runtime is cooperative, no threads
- Match repo style: `Result<T>`, `debug_print`, explicit `alloc`

## Stopping Condition

All 4 components implemented, kernel boots, harness passes (existing cases + new cases for async paths + devmgr), `top` works during spawn, `cat /dev/pts/0` works during VFS load, block-region cap verify works in kernel unit tests.

---

## Current State (from 4 explore agents)

### Component #1: procfs → procmgr (~80% done)
- `procfs.rs` uses `IpcCallFuture` for `list_pids_async` (line 300) and `proc_info_async` (line 332)
- PID-keyed via `PROCMGR_LIST_PIDS_LABEL` (0x4A) and `PROCMGR_PROC_INFO_LABEL` (0x4B)
- session-procmgr has `list_pids_handler` (proc_pid.rs:20) and `proc_info_handler` (proc_pid.rs:35)
- Async runtime wired into VFS: `Runtime::new` (main.rs:348), `poll_ready` (main.rs:352), `spawn` (main.rs:1036,1048), `reply_ep` (main.rs:349)
- Timeout hack (`call_with_reply_buf_timeout`) does NOT exist — already gone
- **Remaining issues:**
  - AGENTS.md §7 forbids exactly this approach — must update
  - Stale doc comment at procfs.rs:7 claims `call_with_reply_buf`
  - Direction B (elf_spawn.rs:482) still uses blocking `ipc::call` for `VFS_DERIVE_CHILD_FD_LABEL` — safe because VFS is async, but should be documented

### Component #2: devfs → drivers (2 blocking edges)
- **BLOCKING: tty read** — main.rs:4250, 3348: `ipc::call_with_reply_buf(ep, ..., TTY_READ_REQUEST_LABEL, ...)`
- **BLOCKING: PTS verb forward** — main.rs:1618: `ipc_call(cluuterm_ep, ...)` for termios/pgrp/winsize
- Already de-fused (fire-and-forget or async-park):
  - tty write — `send_with_payload` (main.rs:2291)
  - PTS write — `send_msg_with_payload` (main.rs:2379), comment cites deadlock avoidance
  - PTS read — async-park pattern (main.rs:3627-3658), comment cites deadlock avoidance

### Component #3: devmgr (greenfield)
- No devmgr code, no references, no TODOs anywhere
- No kernel block cap type:
  - `ObjectRef` (scope.rs:157-173): Thread, Space, Endpoint, Irq, Clock, Frame, Notification, VfsViewManager — no Block
  - `Rights` (rights.rs): READ, WRITE, EXECUTE, CREATE, DESTROY, GRANT, MAP, MANAGE, THREAD_CONTROL, THREAD_SUSPEND, SPACE_MAP/UNMAP/GRANT, IPC_SEND/RECV/CALL/REPLY, IRQ_HANDLE/ACK, PCI_ACCESS — no block right
  - `InvokeOp` (mod.rs:368-457): ~46 ops, no block op
- ext2 is co-resident inside virtio-blk (in-process trait call, not IPC)
- VFS talks to virtio-blk via `FS_READ_GRANT` etc. IPC (file-level)
- Raw-block clients use `BLK_OPEN_SESSION`/`BLK_SUBMIT`/`BLK_COMPLETE` IPC (libcluu/src/ipc.rs:330-334)

### Component #4: Event loop (foundation exists, trait limited)
- `AsyncMountBackend` trait (mount.rs:162-180): only `name`, `open_async`, `readdir_async`
- Only `ProcfsBackend` implements it
- `dispatch_async` (main.rs:991-1062) only handles `VfsOp::Open` and `VfsOp::Readdir`
- All other VfsOps on async mounts fall through to sync path → `Err(InvalidOperation)`
- `VfsCompletion` enum only has `Open` and `Readdir` variants

---

## Phases

### Phase 0: §7 Update + Cleanup (component #1 completion)

**Files:**
- `AGENTS.md` — rewrite §7
- `userspace/vfs/src/procfs.rs` — fix stale doc comment

**§7 new text:**
Replace the sync-only constraint with: "The async runtime in `libcluu::async_runtime` is the canonical deadlock-avoidance mechanism for single-threaded servers. VFS, session-procmgr, and future devmgr use it. Sync `MountBackend` remains for in-process backends (memfs, ext2-via-remote cached reads, devfs null/zero/urandom). All IPC-bound backends must use `AsyncMountBackend`."

**procfs.rs:7:** Update doc comment from `call_with_reply_buf` to `IpcCallFuture` (async).

**Test:** Harness case `l2_top_during_spawn` — run `top` while `spawn` is in progress, verify no hang, verify child appears in top output.

### Phase 1: devfs async edges (component #2)

Convert the 2 remaining blocking devfs edges to use the async runtime.

#### 1a: tty read → async

**Current:** main.rs:4250, 3348 — `ipc::call_with_reply_buf(ep, &req, &[], &mut tty_buf)`

**Target:** Use `IpcCallFuture` to send `TTY_READ_REQUEST_LABEL` non-blocking, await reply via runtime.

**Files:**
- `userspace/vfs/src/main.rs` — `read_grant_device` function (around line 4220)
- `userspace/vfs/src/mount.rs` — `DeviceBackend` impl: add `AsyncMountBackend` impl or add `read_async` method
- `userspace/libcluu/src/async_runtime.rs` — verify `IpcCallFuture` supports payload replies (tty read returns data in reply payload)

**Design decision:** DeviceBackend currently implements `MountBackend` (sync). Two options:
- **Option A:** Make DeviceBackend implement AsyncMountBackend instead (breaking change — all DeviceBackend ops go async)
- **Option B:** Keep DeviceBackend as MountBackend, but convert just the tty read call site to use `IpcCallFuture` directly, park the reply_token, and deliver the reply from the completion queue (like the PTS read async-park pattern but using the runtime)

**Chosen: Option B.** DeviceBackend's null/zero/urandom/fb paths are in-process and don't need async. Only the tty read path crosses a process boundary. Converting just that call site is minimal and matches the existing PTS read pattern.

**Implementation:**
1. In `read_grant_device` for `DeviceType::Tty`/`Tty0`/`Console`:
   - Instead of `ipc::call_with_reply_buf(ep, ...)`, create an `IpcCallFuture`
   - `runtime.spawn(async move { let (reply, payload) = IpcCallFuture::new(ep, req).await?; push_completion(VfsCompletion::TtyRead { reply_token, client_id, reply, payload }); })`
   - Return `Ok(())` without replying (park the reply)
2. Add `VfsCompletion::TtyRead { reply_token, client_id, reply, payload }` variant
3. In the completion drain loop (main.rs:354-365), handle `VfsCompletion::TtyRead` — format the reply and `ipc::reply(reply_token, ...)`

**Test:** Harness case `l2_cat_tty_during_vfs_load` — open `/dev/tty0` for read while VFS is busy with procfs query, verify no hang.

#### 1b: PTS verb forward → async

**Current:** main.rs:1618 — `libcluu::syscall::ipc_call(cluuterm_ep, &send_buf, &mut recv_buf)`

**Target:** Same pattern as 1a — use `IpcCallFuture`, park reply, deliver from completion queue.

**Files:**
- `userspace/vfs/src/main.rs` — PTS verb dispatch (around line 1590-1620)

**Implementation:**
1. For each PTS verb (get_termios, set_termios, get_winsize, set_winsize, set_pgrp):
   - Instead of blocking `ipc_call`, create `IpcCallFuture`
   - `runtime.spawn(async move { let (reply, payload) = IpcCallFuture::new(cluuterm_ep, req).await?; push_completion(VfsCompletion::PtsVerb { reply_token, reply, payload }); })`
   - Return `Ok(())` without replying
2. Add `VfsCompletion::PtsVerb { reply_token, reply, payload }` variant
3. Handle in completion drain loop

**Test:** Harness case `l2_termios_during_proc_query` — run `stty` (termios) while `top` is reading /proc, verify no hang.

### Phase 2: Generalize async VFS trait (component #4)

Extend `AsyncMountBackend` to cover all VfsOps, with default impls that delegate to sync.

#### 2a: Extend the trait

**File:** `userspace/vfs/src/mount.rs`

Add to `AsyncMountBackend`:
```rust
fn stat_async(&self, rel_path: &str, full_path: &str, caller_tid: usize)
    -> Pin<Box<dyn Future<Output = Result<DirEntryStat>> + '_>> { /* default: Err(NotImplemented) */ }
fn read_async(&self, file: &OpenFile, offset: usize, len: usize)
    -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + '_>> { /* default: Err(NotImplemented) */ }
fn write_async(&self, file: &OpenFile, offset: usize, data: &[u8])
    -> Pin<Box<dyn Future<Output = Result<usize>> + '_>> { /* default: Err(NotImplemented) */ }
fn unlink_async(&self, rel_path: &str) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> { /* default */ }
fn mkdir_async(&self, rel_path: &str, mode: usize) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> { /* default */ }
fn rmdir_async(&self, rel_path: &str) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> { /* default */ }
fn rename_async(&self, rel_old: &str, rel_new: &str) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> { /* default */ }
fn link_async(&self, rel_old: &str, rel_new: &str) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> { /* default */ }
fn create_file_async(&self, rel_path: &str, mode: usize) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> { /* default */ }
fn realpath_async(&self, rel_path: &str) -> Pin<Box<dyn Future<Output = Result<String>> + '_>> { /* default */ }
```

Default impls return `Err(NotImplemented)`. Backends override only what they need.

#### 2b: Extend dispatch_async

**File:** `userspace/vfs/src/main.rs`

Extend `dispatch_async` (line 991-1062) to handle all `VfsOp` variants, not just Open/Readdir. Each spawns a task that calls the corresponding `*_async` method and pushes a completion.

#### 2c: Extend VfsCompletion

**File:** `userspace/vfs/src/main.rs`

Add completion variants for each op:
```rust
VfsCompletion::Stat { reply_token, client_id, result }
VfsCompletion::Read { reply_token, client_id, result }
VfsCompletion::Write { reply_token, client_id, result }
VfsCompletion::Unlink { reply_token, result }
VfsCompletion::Mkdir { reply_token, result }
VfsCompletion::Rmdir { reply_token, result }
VfsCompletion::Rename { reply_token, result }
VfsCompletion::Link { reply_token, result }
VfsCompletion::CreateFile { reply_token, result }
VfsCompletion::Realpath { reply_token, result }
VfsCompletion::TtyRead { reply_token, client_id, reply, payload }   // from Phase 1a
VfsCompletion::PtsVerb { reply_token, reply, payload }              // from Phase 1b
```

#### 2d: Extend completion drain loop

**File:** `userspace/vfs/src/main.rs` (line 354-365)

Handle each new completion variant by calling the corresponding `complete_async_*` method.

#### 2e: ProcfsBackend overrides

**File:** `userspace/vfs/src/procfs.rs`

ProcfsBackend should override `stat_async` (it queries procmgr for proc info). Open and Readdir are already async. The rest stay as default `Err(NotImplemented)`.

**Test:** Harness case `l2_proc_stat_async` — `stat /proc/<pid>/stat` during spawn, verify no hang.

### Phase 3: Kernel block-region cap type (component #3 kernel)

#### 3a: New ObjectRef variant

**File:** `kernel/src/token/scope.rs`

Add to `ObjectRef`:
```rust
/// Block-region authority cap.
///
/// `device_id` identifies the block device (assigned by devmgr at registration).
/// `start_sector` and `sector_count` define the region bounds.
/// The kernel verifies these bounds when the token is presented to a driver
/// via `token_verify`. The kernel does NOT perform block I/O — it only
/// attests to the authority.
BlockRegion { device_id: u32, start_sector: u64, sector_count: u64 },
```

#### 3b: Token derivation scoping

**File:** `kernel/src/token/mod.rs` (TokenDeriveScoped handler)

Extend `token_derive_scoped` to handle `BlockRegion` scoping:
- Parent `BlockRegion { device_id, start_sector, sector_count }` can derive a child with a sub-region: `BlockRegion { device_id, start_sector: sub_start, sector_count: sub_count }` where `sub_start >= start_sector && sub_start + sub_count <= start_sector + sector_count`
- This lets devmgr grant a session a sub-region of the full disk

#### 3c: Userspace token verification

**File:** `userspace/libcluu/src/token.rs` (or wherever token_verify is)

Add a helper for drivers to verify a BlockRegion token:
```rust
pub fn verify_block_region(token: usize, expected_device: u32, sector: u64, count: u64) -> Result<()>;
```
This calls `sys_invoke(TokenGetInfo, ...)` to read the token's scope and checks the region bounds.

#### 3d: CapProfile extension

**File:** `userspace/libcluu/src/cap.rs`

Add `BLOCK_REGION` bit to `CapProfile`:
```rust
pub const BLOCK_REGION: u32 = 1 << 5;
```
Processes with this profile may hold `BlockRegion` tokens.

**Test:** Kernel unit test — derive a BlockRegion token, verify scope bounds, attempt out-of-bounds derivation fails.

### Phase 4: devmgr userspace service (component #3 userspace)

#### 4a: devmgr crate

**New crate:** `userspace/devmgr/`

Structure:
```
userspace/devmgr/
├── Cargo.toml
├── Cluufile
└── src/
    ├── main.rs          — event loop (uses async runtime)
    ├── registry.rs      — device registry (device_id → driver endpoint)
    ├── region.rs        — region allocation (mint BlockRegion tokens for sessions)
    └── ipc.rs           — IPC protocol (DEVMGR_REGISTER, DEVMGR_GRANT_REGION, ...)
```

**devmgr responsibilities:**
1. At boot, register as "devmgr" in the registry
2. Accept device registrations from block drivers (virtio-blk registers its disk)
3. At session creation (called by procmgr), mint a `BlockRegion` token for the session covering the filesystem partition
4. The session-VFS (or a per-session block client) uses the token to read/write blocks via the driver, with the driver verifying the token

#### 4b: IPC protocol

**File:** `userspace/libcluu/src/ipc.rs`

New labels:
```rust
pub const DEVMGR_REGISTER_LABEL: u32 = 0x500;    // driver → devmgr: register a block device
pub const DEVMGR_GRANT_REGION_LABEL: u32 = 0x501; // procmgr → devmgr: grant region for a session
pub const DEVMGR_REVOKE_LABEL: u32 = 0x502;       // procmgr → devmgr: revoke session's region
```

#### 4c: virtio-blk integration

**File:** `userspace/virtio-blk/src/main.rs`

Add: at boot, virtio-blk registers with devmgr (sends `DEVMGR_REGISTER_LABEL` with device geometry).

Add: `BLK_SUBMIT` handler verifies the caller's BlockRegion token before servicing:
```rust
// In BLK_SUBMIT handler:
let token = msg.words[0]; // caller's BlockRegion token
let sector = msg.words[1];
let count = msg.words[2];
libcluu::token::verify_block_region(token, self.device_id, sector, count)?;
// proceed with read/write
```

#### 4d: procmgr integration

**File:** `userspace/root-procmgr/src/main.rs` and `userspace/session-procmgr/src/main.rs`

At session creation, procmgr calls devmgr to grant the session a BlockRegion token. The token is passed to the session-VFS via the spawn envelope.

#### 4e: Session-VFS block client

**File:** `userspace/vfs/src/main.rs` or new `userspace/vfs/src/blk_client.rs`

Session-VFS receives a BlockRegion token at spawn. For ext2 reads, instead of talking to virtio-blk via `FS_READ_GRANT` (file-level), the session-VFS can optionally use `BLK_SUBMIT` with its BlockRegion token (block-level). This enables per-session filesystem isolation.

**Initial scope:** Just wire the token through. Don't change the ext2 read path yet — keep the `FS_READ_GRANT` path. The BlockRegion token is available for future per-session ext2. This keeps Phase 4 manageable.

**Test:** Harness case `l2_devmgr_grant` — boot, verify devmgr registers, verify session creation grants a BlockRegion token, verify the token's scope bounds are correct.

### Phase 5: Integration + Full Test Suite

#### 5a: Full harness run

Run all existing harness cases to verify no regressions:
```bash
python -m cluu_harness --no-build
```

#### 5b: New test cases

Add harness cases:
1. `l2_top_during_spawn` — top + spawn concurrency (Phase 0)
2. `l2_cat_tty_during_vfs_load` — tty read + procfs concurrency (Phase 1a)
3. `l2_termios_during_proc_query` — PTS verb + procfs concurrency (Phase 1b)
4. `l2_proc_stat_async` — stat /proc/<pid>/stat during spawn (Phase 2e)
5. `l2_devmgr_grant` — devmgr registration + region grant (Phase 4)
6. `l2_devmgr_revoke` — region revocation at session exit (Phase 4)

#### 5c: Kernel unit tests

```bash
rustc --edition 2021 --test kernel/src/token/scope.rs -o /tmp/t && /tmp/t
rustc --edition 2021 --test kernel/src/token/mod.rs -o /tmp/t && /tmp/t
```

#### 5d: KB updates

Update knowledge base notes:
- `patterns/cluu/cluu-async-runtime-no-std.md` — add generalized trait, devfs edges
- `decisions/cluu/cluu-per-session-vfs-architecture.md` — update devmgr status from "deferred" to "implemented"
- New: `patterns/cluu/cluu-block-region-cap.md` — kernel BlockRegion cap pattern
- New: `concepts/cluu/cluu-devmgr.md` — devmgr architecture

---

## Execution Order

```
Phase 0 (§7 + cleanup) ──────────────────────────► verify boot
Phase 1a (tty read async) ──────────────────────► verify tty read
Phase 1b (PTS verb async) ──────────────────────► verify termios
Phase 2 (generalize trait) ─────────────────────► verify all ops
Phase 3 (kernel BlockRegion) ───────────────────► verify kernel tests
Phase 4 (devmgr service) ───────────────────────► verify boot + grant
Phase 5 (integration + tests) ──────────────────► full harness
```

Phases 0–2 are userspace-only, low risk. Phase 3 touches the kernel — highest risk, needs careful testing. Phase 4 builds on Phase 3. Phase 5 is verification.

## Delegation Plan

| Phase | Delegate? | Category | Why |
|-------|-----------|----------|-----|
| 0 | Direct | — | 2 small edits |
| 1a | Delegate | unspecified-high | Multi-file, async pattern, VFS internals |
| 1b | Delegate | unspecified-high | Same pattern, different call site |
| 2 | Delegate | deep | Trait design, dispatch refactor, many variants |
| 3 | Direct + Oracle | — | Kernel is near-freeze, needs careful hand-coding + Oracle review |
| 4 | Delegate | deep | New crate, IPC protocol, integration |
| 5 | Direct | — | Verification, evidence gathering |

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| Async runtime has bugs (completion queue, cookie correlation) | Already tested via procfs; extend tests incrementally |
| Kernel BlockRegion breaks freeze invariant | Minimal addition — new ObjectRef variant only, no new syscalls, no new InvokeOp |
| devmgr integration breaks boot | Phase 4 is additive — devmgr is a new service, doesn't change existing boot path until wired |
| Generalized trait causes dispatch regressions | Phase 2 keeps sync path for sync backends; async path only for async backends |
| tty read async changes user-visible behavior | Same reply format, just non-blocking delivery — transparent to callers |

## What This Plan Does NOT Do

- Does not make session-procmgr async (Direction B of the deadlock). VFS being async (Direction A) already breaks the deadlock. Making procmgr async is a separate effort.
- Does not split ext2 out of virtio-blk. ext2 stays co-resident. BlockRegion tokens are minted but the ext2 read path stays `FS_READ_GRANT` (file-level). Per-session ext2 is future work.
- Does not add new syscalls. BlockRegion is a new ObjectRef variant verified via existing `TokenGetInfo` InvokeOp. No new InvokeOp needed — the kernel only attests to authority, the driver does the I/O.
- Does not change the root-VFS /proc path (stays TID-keyed, admin view, deferred per KB decision).
