# async-vfs-pid-proc — Draft

## Status: awaiting-approval

## Pending action: write .omo/plans/async-vfs-pid-proc.md (skeleton created, todos pending)

## Approach

### Problem
Session-VFS single-threaded event loop does blocking IPC to session-procmgr
for `/proc/<tid>/stat` queries. Session-procmgr single-threaded loop does
blocking IPC to session-VFS for `derive_child_fd` during spawn. Mutual
deadlock. Current hack: 100ms timeout (wrong — masks the issue, drops data).

### Root cause (two-fold)
1. **Control flow**: single-threaded servers doing blocking IPC to each other
   = deadlock class. Fix: async event loop in VFS (and later procmgr).
2. **Data model**: `/proc` keyed by TID (kernel concept) but top wants
   processes (PID, procmgr concept). VFS does TID→PID translation via
   blocking IPC. Fix: rekey `/proc` to PID, query procmgr directly.

### Architecture decision: dual trait
- `MountBackend` (sync) stays unchanged — ext2, initrd, devfs, pts keep
  kernel-level sync ops (fast, no IPC).
- New `AsyncMountBackend` trait — object-safe, returns
  `Pin<Box<dyn Future>>`. procfs implements this.
- Mount table holds `enum AnyMount { Sync(Box<dyn MountBackend>), Async(Box<dyn AsyncMountBackend>) }`.
- VFS server loop: sync backends called inline; async backends spawned as
  tasks.

### Architecture decision: async runtime in libcluu
- Location: `libcluu/src/async_runtime/` (new module)
- Single-threaded executor, no `Send` bounds, no locks for task state
- Waker: ready-queue based (not notification-backed — simpler, few tasks)
- IPC bridge: `IpcCallFuture` — `ipc_send_nonblocking` + cookie correlation
  + reply delivered by executor's recv loop
- Runtime accessed via thread-local `current()` set by executor before poll
- API: `Runtime::new()`, `spawn(fut)`, `run_until_idle()`, `block_on(fut)`

### Architecture decision: PID-keyed /proc (session-scoped)
- `readdir("/proc")` → async call `session-procmgr.list_pids()` → PID list
- `open("/proc/<pid>/stat")` → async call `session-procmgr.proc_info(pid)`
- `thread_enumerate` no longer used for session /proc (stays for root-VFS
  admin view — separate concern, deferred)
- session-procmgr: new `PROCMGR_LIST_PIDS_LABEL` + `PROCMGR_PROC_INFO_LABEL`
  handlers (PID-keyed, O(log n) via `by_pid` BTreeMap)

### Components (topology lock)
1. **libcluu async runtime** — executor, task, waker, IpcCallFuture
2. **AsyncMountBackend trait + AnyMount enum** — mount.rs, object-safe
3. **VFS server async event loop** — main.rs run_vfs() converted to executor
4. **procfs PID-keyed async** — procfs.rs implements AsyncMountBackend
5. **session-procmgr PID API** — list_pids + proc_info handlers
6. **Cleanup + KB** — revert timeout hack, KB notes, regression tests

### Key design points
- `Pin<Box<dyn Future>>` for object-safe async trait (nightly 1.94, alloc OK)
- Cookie correlation: reuse `copy_call_cookie` (ipc.rs:866) — reply cookie
  matches request cookie
- Executor loop: poll ready tasks → if all pending, `ipc_recv_any` on
  `[vfs_ep, registry_ep]` → match reply cookie → wake task OR spawn new
- Sync backends never block the executor (kernel block caps, initrd memory)
- `IpcCallFuture` states: NotSent → send_nonblocking → Waiting → reply
  delivered → Ready

## Findings (from exploration)

### VFS (bg_9fbd36b3)
- Main loop: main.rs:339, `ipc_recv_any_with_sender(&[ep, registry_ep], buf, u64::MAX)`
- Single-threaded, `VfsServer::handle_message` at main.rs:834, `&mut self`
- `VfsServer::new` at main.rs:756 (14 params), `setup_mounts` at main.rs:368
- ProcfsBackend: procfs.rs:238, `procmgr_endpoint: usize` only field
- `query_tid_list`: procfs.rs:310, calls `thread_enumerate` (kernel, fast)
- `query_procmgr`: procfs.rs:253, `call_with_reply_buf_timeout(ep, req, &[], &mut buf, 100)` at line 266
- `open` at procfs.rs:323, `readdir` at procfs.rs:350 — no `read` override (data inline in OpenFile::Virtual)
- MountBackend trait: mount.rs:72, `Send + Sync`, methods: name/open/readdir/stat_by_path/read/...
- NO async/await anywhere in userspace (5 false-positive hits only)

### session-procmgr (bg_10064708)
- ProcQuery handler: proc_query.rs:37-130, TID→PID via O(n) linear scan (proc_query.rs:54-57)
- child_table: child_table.rs:33-38, `by_pid: BTreeMap<Pid, ChildState>` — PID is primary key
- ChildState: child_table.rs:7-31, has `pid, child_tid, argv0, parent_pid, thread_tok, space_tok, ...`
- ProcQueryLocal: proc_query_local.rs:13-33, already does PID-list dump to ProcInfo wire struct
- Deadlock source: elf_spawn.rs:482, `libcluu::ipc::call(session-VFS)` for derive_child_fd, blocking
- Main loop: main.rs:200-260, single-threaded
- root-procmgr fan-out: main.rs:3281, blocking `call_with_reply_buf` to session_pmgr_endpoints

### libcluu IPC (bg_42918d82)
- `#![no_std]` (lib.rs:17), `extern crate alloc` (lib.rs:23)
- `ipc_send_nonblocking`: syscall.rs:342 — true fire-and-forget, swallows WouldBlock
- `ipc_recv_nonblocking`: syscall.rs:497 — timeout=0
- `ipc_recv_any`: syscall.rs:418 — multi-endpoint multiplexing recv with timeout
- `notification_create/signal/wait/poll`: syscall.rs:1499-1523 — seL4-style bitset (available but not used in initial design)
- `endpoint_peek`: syscall.rs:819 — readiness without consumption
- `copy_call_cookie`: ipc.rs:866 — reply cookie matching
- `extract_reply_id`: ipc.rs:826 — extract cookie from reply
- `spin::Mutex` available (Cargo.toml dep)
- `core::task::{Future, Waker, Context, Poll}` — all no_std stable
- NO async runtime crate deps (no tokio/embassy/futures)

### Rust toolchain
- nightly 1.94.0, edition 2021
- RPITIT + async fn in traits stable since 1.75
- `dyn AsyncMountBackend` needs `Pin<Box<dyn Future>>` returns (object-safe)

## Approval gate

User approved: dual trait (AsyncMountBackend sibling), async runtime in
libcluu, PID-keyed /proc, async pull model. Scope: VFS server + procfs +
session-procmgr + libcluu runtime. Root-procmgr fan-out deferred. Root-VFS
admin /proc deferred.

## Open questions (resolved)
- MountBackend async scope: DUAL TRAIT (user decision)
- Runtime location: libcluu::async_runtime (user directive)
- Waker mechanism: ready-queue (not notification — simpler, sufficient)
- /proc key: PID (not TID — procmgr owns procs, kernel owns threads)
- root-procmgr changes: none (not on session-VFS path in new design)
