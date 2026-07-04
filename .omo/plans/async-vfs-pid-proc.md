# async-vfs-pid-proc - Work Plan

## TL;DR (For humans)
<!-- Fill this LAST, after the detailed plan below is written, so it summarizes the REAL plan. -->
<!-- Plain English for a non-engineer: NO file paths, NO todo numbers, NO wave/agent/tool names. -->

**What you'll get:** A single-threaded async runtime in libcluu that lets VFS handle multiple client requests concurrently without blocking on downstream IPC. VFS's `/proc` filesystem switches from thread-ID-keyed (which required blocking IPC to procmgr for TID→PID translation) to process-ID-keyed (querying procmgr directly). The session-VFS ↔ session-procmgr deadlock is eliminated: VFS sends async IPC to procmgr and keeps serving other clients while waiting for the reply.

**Why this approach:** (1) PID-keyed `/proc` matches cluu's ownership model — procmgr owns processes (PIDs), kernel owns threads (TIDs). VFS was doing TID→PID translation via blocking IPC, which was architecturally wrong and caused the deadlock. (2) A dual-trait mount system (`MountBackend` sync, `AsyncMountBackend` async) lets only procfs go async while ext2/initrd/devfs/pts stay sync — minimal blast radius. (3) The runtime lives in libcluu so compositor, session-procmgr, and future devmgr can adopt it later without redesign.

**What it will NOT do:** No kernel changes (kernel is near-freeze). No converting existing sync backends to async. No session-procmgr async refactor (stays sync, just gets new PID-keyed handlers). No root-procmgr or root-VFS changes. No timeout hacks — the 100ms timeout is removed entirely.

**Effort:** Large
**Risk:** Medium — async runtime is new infrastructure; the `&'static` lifetime extension for backend refs in spawned tasks is sound but needs care.
**Decisions to sanity-check:** (1) Dual-trait vs converting all backends (chose dual-trait — smaller blast radius). (2) Cookie correlation in `words[5]` + reply endpoint in `words[4]` (reuses existing convention). (3) `IpcCallFuture` uses `ipc_send` (returns `Err(WouldBlock)` for retry) not `ipc_send_nonblocking` (silently drops messages).

Your next move: approve to start execution, or request a high-accuracy review first. Full execution detail follows below.

---

> TL;DR (machine): Large effort, Medium risk — async runtime in libcluu + dual-trait mount system + PID-keyed /proc + session-procmgr PID API; 6 todos in 3 waves.

## Scope
### Must have
1. **Async runtime in `libcluu::async_runtime`** — single-threaded, no_std, alloc-based executor with task spawning, IPC bridge via `IpcCallFuture` (non-blocking `ipc_send` + cookie correlation + reply delivery via dedicated endpoint)
2. **`AsyncMountBackend` trait** — object-safe sibling to `MountBackend`, methods return `Pin<Box<dyn Future>>`. `AnyMount` enum in `MountTable` to hold either type.
3. **VFS server async event loop** — `run_vfs()` main loop converted to executor: polls ready tasks, `ipc_recv_any` on `[vfs_ep, registry_ep, reply_ep]`, routes replies by cookie, dispatches new requests (sync inline, async spawns task)
4. **procfs PID-keyed async** — `/proc` readdir lists PIDs (async call to procmgr `list_pids`), `/proc/<pid>/stat` async call to procmgr `proc_info(pid)`. `ProcfsBackend` implements `AsyncMountBackend`.
5. **session-procmgr PID API** — new `PROCMGR_LIST_PIDS_LABEL` + `PROCMGR_PROC_INFO_LABEL` handlers, reply via `ipc_send` to embedded reply endpoint (not `ipc_reply`). PID-keyed via `by_pid` BTreeMap (O(log n)).
6. **Cleanup** — revert `call_with_reply_buf_timeout(100ms)` hack in procfs.rs, remove `call_with_reply_buf_timeout` from libcluu/src/ipc.rs if no other callers. KB notes for async runtime pattern + PID-keyed /proc decision.

### Must NOT have (guardrails, anti-slop, scope boundaries)
- **NO kernel changes** — the kernel is near-freeze. Use existing syscalls (`ipc_send`, `ipc_recv_any`, `endpoint_create`).
- **NO `async fn` in traits without object safety** — use `Pin<Box<dyn Future>>` returns for `dyn AsyncMountBackend`.
- **NO `Send` bounds on futures** — single-threaded executor. `Sync` only (for `&self` across `.await`).
- **NO converting existing sync backends** (RemoteBackend, InitrdBackend, DeviceBackend, PtsBackend, MemFsBackend) to async. They stay on `MountBackend`. Only `ProcfsBackend` moves to `AsyncMountBackend`.
- **NO session-procmgr async refactor** — session-procmgr stays single-threaded sync. Only new handlers added.
- **NO root-procmgr changes** — root-procmgr's fan-out path is not on session-VFS's path in the new design.
- **NO root-VFS /proc changes** — root-VFS admin /proc stays TID-keyed (separate concern, deferred).
- **NO `ipc_send_nonblocking`** for RPC — it silently drops messages on WouldBlock. Use `ipc_send` which returns `Err(WouldBlock)` for retry.
- **NO `as any`, `@ts-ignore`** (Rust equivalent: no `unsafe` transmute except the one documented lifetime extension for `&'static dyn AsyncMountBackend`).
- **NO timeout hacks** — the 100ms timeout in procfs.rs is removed, not kept as fallback.

## Verification strategy
> Zero human intervention - all verification is agent-executed.
- Test decision: tests-after (integration via QEMU harness). Unit tests for runtime logic where possible.
- Evidence: .omo/evidence/task-<N>-async-vfs-pid-proc.<ext>
- Build: `cargo xtask build` must succeed
- Boot: `cargo xtask run` — QEMU boots, login prompt appears
- Regression: `bash scripts/harness_suite.sh` — existing tests still pass
- Deadlock verification: spawn micropython in second cluuterm while `top` is running — no hang

## Execution strategy
### Parallel execution waves
> Wave 1: T1, T2, T3 (no deps, parallel). Wave 2: T4, T5 (depend on Wave 1). Wave 3: T6 (depends on all).

### Dependency matrix
| Todo | Depends on | Blocks | Can parallelize with |
| --- | --- | --- | --- |
| T1 (async runtime) | none | T4, T5 | T2, T3 |
| T2 (AsyncMountBackend trait) | none | T4, T5 | T1, T3 |
| T3 (session-procmgr PID API) | none | T5 | T1, T2 |
| T4 (VFS async event loop) | T1, T2 | T6 | T5 |
| T5 (procfs PID-keyed async) | T2, T3 | T6 | T4 |
| T6 (cleanup + KB + regression) | T4, T5 | none | none |

## Todos
> Implementation + Test = ONE todo. Never separate.
<!-- APPEND TASK BATCHES BELOW THIS LINE WITH edit/apply_patch - never rewrite the headers above. -->

- [ ] 1. libcluu async runtime — executor, task, waker, IpcCallFuture
  What to do: Create `userspace/libcluu/src/async_runtime/mod.rs` (new module). Implement a single-threaded, no_std, alloc-based async executor. Components:
    - `Runtime` struct: `tasks: Vec<Task>`, `pending_ipc: BTreeMap<usize, TaskId>` (cookie→task), `ready_queue: VecDeque<TaskId>`, `next_task_id: usize`, `next_cookie: usize`, `reply_endpoint: usize`
    - `Task` struct: `id: TaskId`, `future: Pin<Box<dyn Future<Output = ()>>>`
    - `Runtime::new(token_self: usize) -> Self` — creates reply endpoint via `syscall::endpoint_create(token_self)` (syscall.rs:1270)
    - `Runtime::spawn(&mut self, fut: impl Future<Output = ()> + 'static)` — box future, assign task_id, push to ready_queue
    - `Runtime::poll_ready(&mut self)` — drain ready_queue: poll each task, handle completion (remove from tasks vec)
    - `Runtime::has_pending(&self) -> bool` — any tasks alive or pending_ipc non-empty
    - `Runtime::reply_endpoint(&self) -> usize` — accessor for recv loop integration
    - `Runtime::deliver_reply(&mut self, cookie: usize, msg: Message, payload_len: usize)` — find pending task by cookie, store reply data, wake task, push to ready_queue
    - Thread-local `current_runtime: *mut Runtime` — set before polling tasks, cleared after. Accessed by `IpcCallFuture::poll` to register waker.
    - `IpcCallFuture` struct: `endpoint: usize`, `request: Message`, `reply_buf: Box<[u8]>`, `cookie: usize`, `state: IpcCallState` (NotSent/Waiting/Done), `reply: Option<(Message, usize)>`
    - `IpcCallFuture::new(endpoint: usize, request: Message) -> Self` — allocates reply_buf (4096 bytes), gets cookie from `current_runtime.next_cookie++`
    - `impl Future for IpcCallFuture`: `Output = Result<(Message, usize)>`. Poll: NotSent→`syscall::ipc_send(endpoint, request.as_bytes())` (syscall.rs:294). On Ok→state=Waiting, register cookie in `current_runtime.pending_ipc`, return Pending. On `Err(WouldBlock)`→`cx.waker().wake_by_ref()`, return Pending. On other Err→Ready(Err). Waiting→check `self.reply`, if Some→Ready(Ok), if None→store waker from cx, return Pending.
    - Request message wire format: `words[4] = reply_endpoint` (from runtime), `words[5] = cookie`. These are read by procmgr to route the reply.
    - Module declaration: add `pub mod async_runtime;` to `userspace/libcluu/src/lib.rs` after existing module declarations (~line 50).
  Must NOT do: NO `Send` bounds. NO `ipc_send_nonblocking` (drops messages). NO blocking `ipc_call` inside futures. NO `std` dependency. NO notification-based wakers (ready-queue only, simpler).
  Parallelization: Wave 1 | Blocked by: none | Blocks: T4, T5 | Can parallelize with: T2, T3
  References (executor has NO interview context):
    - `userspace/libcluu/src/lib.rs:17` — `#![no_std]`, `:23` — `extern crate alloc`
    - `userspace/libcluu/src/lib.rs:50` — module declarations area (add `pub mod async_runtime;`)
    - `userspace/libcluu/src/syscall.rs:294` — `ipc_send(endpoint, msg)` returns `Err(WouldBlock)` if queue full (propagated via `?` at line 337)
    - `userspace/libcluu/src/syscall.rs:1270` — `endpoint_create(root_token) -> Result<usize>`
    - `userspace/libcluu/src/syscall.rs:418` — `ipc_recv_any(tokens, buf, timeout_ms)` — timeout=0 is non-blocking
    - `userspace/libcluu/src/syscall.rs:427` — `ipc_recv_any_with_sender(tokens, buf, timeout_ms)` — returns (index, len, sender_tid)
    - `userspace/libcluu/src/types.rs:82-97` — Message/MessageTag layout. words[4] and words[5] available for reply_ep + cookie.
    - `userspace/libcluu/src/ipc.rs:866` — `copy_call_cookie` pattern (for reference on cookie convention)
    - `userspace/libcluu/Cargo.toml` — deps: spin, lazy_static, alloc available
    - `core::future::Future`, `core::task::{Context, Waker, Poll, RawWaker, RawWakerVTable}` — all stable no_std
    - `alloc::boxed::Box`, `alloc::collections::VecDeque`, `alloc::collections::BTreeMap` — all available
  Acceptance criteria (agent-executable): `cargo xtask build` succeeds. New module compiles. No warnings (run `cargo clippy` on libcluu). `libcluu::async_runtime::Runtime` is public and constructible.
  QA scenarios: 
    - Happy: `cargo xtask build` → exit 0. Evidence: .omo/evidence/task-1-async-vfs-pid-proc-build.log
    - Failure: `cargo clippy --target x86_64-unknown-linux-gnu -p libcluu` → no errors (warnings OK). Evidence: .omo/evidence/task-1-async-vfs-pid-proc-clippy.log
  Commit: Y | feat(libcluu): add single-threaded async runtime with IPC bridge

- [ ] 2. AsyncMountBackend trait + AnyMount enum in mount.rs
  What to do: Add `AsyncMountBackend` trait and `AnyMount` enum to `userspace/vfs/src/mount.rs`. Changes:
    - Add `AsyncMountBackend` trait (after existing `MountBackend` at line 152):
      ```rust
      pub trait AsyncMountBackend: Sync {
          fn name(&self) -> &'static str;
          fn open_async(&self, rel_path: &str, full_path: &str, caller_tid: usize)
              -> Pin<Box<dyn Future<Output = Result<OpenFile>> + '_>>;
          fn readdir_async(&self, rel_path: &str, caller_tid: usize)
              -> Pin<Box<dyn Future<Output = Result<Vec<DirEntry>>> + '_>>;
      }
      ```
    - Add `AnyMount` enum:
      ```rust
      pub enum AnyMount {
          Sync(Box<dyn MountBackend>),
          Async(Box<dyn AsyncMountBackend>),
      }
      ```
    - Add `use core::pin::Pin;` and `use core::future::Future;` to mount.rs imports (after line 15)
    - Change `Mount` struct (line 731): `backend: Box<dyn MountBackend>` → `backend: AnyMount`
    - Change `MountTable::mount()` (line 747): accept `AnyMount` instead of `Box<dyn MountBackend>`. Add convenience `mount_sync(prefix, Box<dyn MountBackend>)` and `mount_async(prefix, Box<dyn AsyncMountBackend>)` methods.
    - Add `MountTable::get_async_backend(&self, path: &str) -> Option<&dyn AsyncMountBackend>`: resolve path, if AnyMount::Async return Some, else None
    - Add `MountTable::is_async(&self, path: &str) -> bool`: resolve path, return true if AnyMount::Async
    - Update existing `mount_initrd`, `mount_remote`, `mount_virtual` to wrap in `AnyMount::Sync`
    - Update `MountTable::open()`, `readdir()`, `stat_by_path()` etc. — if AnyMount::Async, return `Err(InvalidOperation)` (sync API not available for async backends). These are called by the sync path only; async backends are handled by VFS server's async dispatch.
    - Existing sync backends (InitrdBackend, RemoteBackend, DeviceBackend, VirtualBackend, MemFsBackend) stay on `MountBackend` — no changes to their impls.
  Must NOT do: NO `Send` bound on `AsyncMountBackend` or futures. NO converting existing backends. NO async methods on `MountBackend` itself. NO removing existing `MountBackend` methods.
  Parallelization: Wave 1 | Blocked by: none | Blocks: T4, T5 | Can parallelize with: T1, T3
  References:
    - `userspace/vfs/src/mount.rs:72-152` — existing `MountBackend` trait (all methods)
    - `userspace/vfs/src/mount.rs:731-734` — `Mount` struct with `backend: Box<dyn MountBackend>`
    - `userspace/vfs/src/mount.rs:737-749` — `MountTable` struct + `mount()` method
    - `userspace/vfs/src/mount.rs:752-774` — `mount_initrd`, `mount_remote`, `mount_virtual` convenience methods
    - `userspace/vfs/src/mount.rs:777-786` — `MountTable::open()` and `readdir()` (sync dispatch)
    - `userspace/vfs/src/mount.rs:888-917` — `resolve()` method (path → mount + rel_path)
    - `userspace/vfs/src/mount.rs:858-860` — `get_backend()` (pattern to follow for `get_async_backend`)
    - `userspace/vfs/src/main.rs:389` — `mounts.mount("/proc", Box::new(procfs::ProcfsBackend::new(...)))` — will change to `mounts.mount_async("/proc", Box::new(...))`
  Acceptance criteria: `cargo xtask build` succeeds. `mount.rs` compiles with new trait + enum. All existing backends still compile unchanged.
  QA scenarios:
    - Happy: `cargo xtask build` → exit 0. Evidence: .omo/evidence/task-2-async-vfs-pid-proc-build.log
    - Failure: introduce a type mismatch (e.g. mount_async with a Sync backend) → compile error (expected). Evidence: .omo/evidence/task-2-async-vfs-pid-proc-typecheck.txt
  Commit: Y | feat(vfs): add AsyncMountBackend trait + AnyMount enum for dual-trait mount system

- [ ] 3. session-procmgr PID-keyed API — list_pids + proc_info handlers
  What to do: Add two new IPC handlers to session-procmgr that are PID-keyed (not TID-keyed) and reply via `ipc_send` to an embedded reply endpoint (not `ipc_reply`). Changes:
    - New label constants (add to `userspace/libs/procmgr-common/src/wire.rs` or `userspace/libcluu/src/ipc.rs` near existing `PROCMGR_PROC_QUERY_LABEL` at line 157):
      ```rust
      pub const PROCMGR_LIST_PIDS_LABEL: u32 = <next available, e.g. 0x4A>;
      pub const PROCMGR_PROC_INFO_LABEL: u32 = <next available, e.g. 0x4B>;
      ```
    - New handler module: `userspace/session-procmgr/src/proc_pid.rs` (new file)
    - `list_pids_handler(state: &SessionState, msg: &Message) -> Result<()>`:
      - Extract `reply_ep = msg.words[4]`, `cookie = msg.words[5]`
      - Iterate `state.child_table.iter()` (child_table.rs:93) → collect all PIDs
      - Serialize as `Vec<u32>` (postcard or raw bytes)
      - Build reply Message: label=PROCMGR_LIST_PIDS_LABEL, words[0]=errno(0), words[1]=payload_len, words[5]=cookie
      - `syscall::ipc_send(reply_ep, reply_bytes)` — NOT `ipc_reply`
    - `proc_info_handler(state: &SessionState, msg: &Message) -> Result<()>`:
      - Extract `pid = msg.words[0]`, `reply_ep = msg.words[4]`, `cookie = msg.words[5]`
      - `state.child_table.lookup_by_pid(Pid::from(pid))` (child_table.rs:71) — O(log n)
      - Build `ProcInfo` wire struct (wire.rs:83-90) from `ChildState` (child_table.rs:7-31): map pid, child_tid, argv0, parent_pid, thread_tok, space_tok
      - Serialize via postcard
      - Build reply Message with cookie in words[5], `ipc_send(reply_ep, reply_bytes)`
    - Register handlers in `userspace/session-procmgr/src/dispatch.rs` dispatch table (after line 125):
      ```rust
      msg.tag.label == PROCMGR_LIST_PIDS_LABEL => crate::proc_pid::list_pids_handler(state, msg),
      msg.tag.label == PROCMGR_PROC_INFO_LABEL => crate::proc_pid::proc_info_handler(state, msg),
      ```
    - These handlers do NOT return a `Reply` for the main loop to `ipc_reply`. They directly `ipc_send` and return `Ok(())`. The main loop (main.rs:235-239) must handle this: if the handler already replied, skip `send_reply`. Add a flag or use a `Reply::AlreadySent` variant.
    - Alternative simpler approach: the handler returns a special `Reply::AsyncReply { reply_ep: usize, msg: Message, payload: Vec<u8> }` variant, and the main loop sends it via `ipc_send` instead of `ipc_reply`. This keeps the dispatch uniform.
  Must NOT do: NO changes to existing `ProcQuery` handler (proc_query.rs). NO changes to `ProcQueryLocal` handler. NO async runtime in session-procmgr. NO new kernel syscalls. NO `ipc_reply` for these handlers (no reply channel from `ipc_send`).
  Parallelization: Wave 1 | Blocked by: none | Blocks: T5 | Can parallelize with: T1, T2
  References:
    - `userspace/session-procmgr/src/dispatch.rs:58-128` — dispatch table (match msg.label arms)
    - `userspace/session-procmgr/src/dispatch.rs:16-34` — SessionState struct (has child_table, session_vfs_cap, etc.)
    - `userspace/session-procmgr/src/child_table.rs:33-38` — ChildTable struct (by_pid: BTreeMap<Pid, ChildState>)
    - `userspace/session-procmgr/src/child_table.rs:7-31` — ChildState fields (pid, child_tid, argv0, parent_pid, thread_tok, space_tok, ...)
    - `userspace/session-procmgr/src/child_table.rs:71-73` — lookup_by_pid (O(log n))
    - `userspace/session-procmgr/src/child_table.rs:93-95` — iter() yields by_pid.values()
    - `userspace/session-procmgr/src/proc_query_local.rs:13-33` — existing PID-list dump pattern (ProcQueryLocal)
    - `userspace/session-procmgr/src/main.rs:200-260` — main recv loop
    - `userspace/session-procmgr/src/main.rs:235-239` — dispatch + send_reply
    - `userspace/libs/procmgr-common/src/wire.rs:83-90` — ProcInfo wire struct
    - `userspace/libcluu/src/ipc.rs:157` — PROCMGR_PROC_QUERY_LABEL=37 (add new labels nearby)
    - `userspace/libcluu/src/syscall.rs:294` — ipc_send (non-blocking, returns WouldBlock)
    - `userspace/libcluu/src/ipc.rs:645` — high-level `send()` wrapper
  Acceptance criteria: `cargo xtask build` succeeds. New handlers compile. Labels are unique (grep for conflicts). Handler logic is testable: can be unit-tested by constructing a mock SessionState with child_table entries.
  QA scenarios:
    - Happy: `cargo xtask build` → exit 0. Evidence: .omo/evidence/task-3-async-vfs-pid-proc-build.log
    - Failure: send PROCMGR_PROC_INFO_LABEL with invalid PID → handler returns errno in reply, no panic. Evidence: .omo/evidence/task-3-async-vfs-pid-proc-error-handling.txt
  Commit: Y | feat(session-procmgr): add PID-keyed list_pids + proc_info handlers with async reply

- [ ] 4. VFS server async event loop — executor integration in main.rs
  What to do: Convert `run_vfs()` (main.rs:203) main loop from a bare `loop { recv → dispatch }` to an executor-integrated loop that handles both sync and async dispatch. Changes:
    - `run_vfs()` (main.rs:203): create `Runtime` after VfsServer construction:
      ```rust
      let token_self = process_info().tokens[TOKEN_SELF];
      let mut runtime = async_runtime::Runtime::new(token_self);
      ```
    - Main loop (main.rs:339-352) becomes:
      ```rust
      loop {
          // 1. Poll ready tasks
          runtime.poll_ready();
          // 2. Recv on [vfs_ep, registry_ep, runtime.reply_endpoint()]
          let tokens = [endpoint, registry_endpoint, runtime.reply_endpoint()];
          let timeout = if runtime.has_pending() { 1 } else { u64::MAX };
          match ipc_recv_any_with_sender(&tokens, &mut buf, timeout) {
              Ok((index, len, sender_tid)) => {
                  if index == 2 {
                      // Async reply — match cookie, deliver to task
                      if let Some((msg, payload)) = parse_message(&buf[..len]) {
                          let cookie = msg.words[5]; // cookie in words[5]
                          runtime.deliver_reply(cookie, msg, len - size_of::<Message>());
                      }
                  } else if index == 1 {
                      server.handle_registry_message(&msg, payload, sender_tid);
                  } else {
                      // index == 0: client request — dispatch with runtime
                      server.handle_message(&msg, payload, sender_tid, &mut runtime);
                  }
              }
              Err(Timeout | WouldBlock) => { /* loop back, poll tasks */ }
              Err(e) => return Err(e),
          }
      }
      ```
    - `handle_message` (main.rs:834): add `runtime: &mut Runtime` parameter. For each VfsOp dispatch:
      - If `self.mounts.is_async(path)` (e.g. `/proc` path): extract owned data (path: String, reply_token: usize from `extract_reply_id(msg)` at line 889, caller_tid), get `&'static dyn AsyncMountBackend` via `self.mounts.get_async_backend(path)` (lifetime-extend with `unsafe transmute` — safe because VfsServer never drops, single-threaded), `runtime.spawn(async move { ... })`. The async block calls `backend.open_async(path, caller_tid).await` then `ipc::reply(reply_token, reply_msg)`.
      - If sync: current flow unchanged — call `self.mounts.open(path, caller_tid)` inline, `ipc::reply(reply_token, ...)`.
    - `handle_open` (main.rs, the Open handler): check if path is `/proc` or under `/proc/` → async path. Otherwise sync.
    - `handle_readdir` (main.rs, the Readdir handler): same check.
    - `setup_mounts` (main.rs:389): change `mounts.mount("/proc", ...)` to `mounts.mount_async("/proc", Box::new(procfs::ProcfsBackend::new(procmgr_endpoint)))`
    - The `'static` lifetime extension: `let backend: &'static dyn AsyncMountBackend = unsafe { core::mem::transmute(backend_ref) };` — document why this is safe (single-threaded, VfsServer never drops, backends in Box in MountTable in VfsServer).
    - Set thread-local `current_runtime` before `runtime.poll_ready()` and before spawning tasks that may poll immediately. Clear after.
  Must NOT do: NO converting `handle_message` to async — it stays sync, spawns async tasks. NO holding `&mut self` across `.await`. NO `spin::Mutex` around VfsServer (single-threaded, no lock needed — tasks get `&'static` backend refs, not `&mut self`). NO changes to sync backend dispatch paths. NO changes to registry message handling (index==1).
  Parallelization: Wave 2 | Blocked by: T1 (runtime), T2 (AsyncMountBackend) | Blocks: T6 | Can parallelize with: T5
  References:
    - `userspace/vfs/src/main.rs:203` — `run_vfs()` entry
    - `userspace/vfs/src/main.rs:339-352` — current main loop (bare recv-dispatch)
    - `userspace/vfs/src/main.rs:706-752` — VfsServer struct (mounts: MountTable at line 713)
    - `userspace/vfs/src/main.rs:756-771` — VfsServer::new signature
    - `userspace/vfs/src/main.rs:834` — handle_message(&mut self, msg, payload, sender_tid)
    - `userspace/vfs/src/main.rs:889` — `reply_token = extract_reply_id(msg).unwrap_or(self.endpoint)` — reply_token is usize (Copy, owned)
    - `userspace/vfs/src/main.rs:893-916` — VfsOp dispatch (Open/Close/ReadGrant/Readdir/...)
    - `userspace/vfs/src/main.rs:368-408` — setup_mounts (mount point registration)
    - `userspace/vfs/src/main.rs:389` — procfs mount: `mounts.mount("/proc", Box::new(procfs::ProcfsBackend::new(procmgr_endpoint)))`
    - `userspace/vfs/src/mount.rs:858-860` — get_backend() pattern (for get_async_backend)
    - `userspace/libcluu/src/ipc.rs:826-832` — extract_reply_id (reply_token extraction)
    - `userspace/libcluu/src/ipc.rs:841` — reply() function (used by async task to reply to client)
    - `userspace/libcluu/src/syscall.rs:427` — ipc_recv_any_with_sender (3-endpoint recv)
    - `userspace/libcluu/src/process.rs` — process_info() for TOKEN_SELF
  Acceptance criteria: `cargo xtask build` succeeds. QEMU boots (`cargo xtask run`). Login works. `ls /` works (sync backends). `ls /proc` works (async backend — may show empty if procmgr handlers not yet wired, but must not hang). `top` starts (may show no processes yet). No hang during boot.
  QA scenarios:
    - Happy: `cargo xtask run`, login as admin, `ls /` → lists root dir. `ls /proc` → lists proc dir (may be empty/stale). `top` → starts, q to quit. Evidence: .omo/evidence/task-4-async-vfs-pid-proc-qemu-boot.txt
    - Failure: `top` during boot → should not hang. If `/proc` readdir fails (procmgr not ready), should return empty, not hang. Evidence: .omo/evidence/task-4-async-vfs-pid-proc-no-hang.txt
  Commit: Y | feat(vfs): integrate async runtime into server event loop with dual sync/async dispatch

- [ ] 5. procfs PID-keyed async implementation — ProcfsBackend implements AsyncMountBackend
  What to do: Convert `ProcfsBackend` from `MountBackend` to `AsyncMountBackend`. Rekey `/proc` from TID to PID. Changes to `userspace/vfs/src/procfs.rs`:
    - Remove `impl MountBackend for ProcfsBackend` (lines 318-414)
    - Add `impl AsyncMountBackend for ProcfsBackend`:
      - `name()` → `"procfs"` (same)
      - `open_async(rel_path, full_path, caller_tid) -> Pin<Box<dyn Future>>`:
        - Static files (meminfo, cpuinfo, uptime — at current line 327) → same `gen_static` but in `async { ... }.boxed()` (no IPC needed)
        - PID path: `parse_pid_path(rel_path)` → extract PID → create `IpcCallFuture` with `PROCMGR_PROC_INFO_LABEL`, words[0]=pid, words[2]=caller_tid → `.await` → parse reply as ProcInfo → format stat/status/cmdline/comm/exe → `OpenFile::Virtual(VirtualFile { data, path, rights: u64::MAX })`
        - `self` dir → `gen_self_stat(caller_tid)` (needs `thread_enumerate` for caller's TID — this stays sync, it's a kernel call)
        - Return `Box::pin(async move { ... })`
      - `readdir_async(rel_path, caller_tid) -> Pin<Box<dyn Future>>`:
        - Root `/proc`: static files + `self` dir + PID entries via `IpcCallFuture(PROCMGR_LIST_PIDS_LABEL)` → `.await` → parse PIDs → `DirEntry` per PID
        - `self`/`<pid>` subdirs: same as current (static subfile list, no IPC)
        - Return `Box::pin(async move { ... })`
    - Remove `query_tid_list()` (procfs.rs:310-315) — no longer used for session /proc
    - Remove `query_procmgr()` (procfs.rs:253-276) — replaced by `IpcCallFuture`
    - Add `use libcluu::async_runtime::IpcCallFuture;`
    - Add `use alloc::boxed::Box;` and `use core::pin::Pin;` and `use core::future::Future;`
    - `ProcfsBackend::new(procmgr_endpoint)` stays the same (procmgr_endpoint field)
    - The `IpcCallFuture` constructor sets words[4]=reply_ep (from runtime thread-local) and words[5]=cookie automatically
    - Stat formatting: reuse existing format string logic from proc_query.rs:99-107: `"pid (name) state cpu_ticks heap_pages other_pages ppid sid cid pcid"`. But now the data comes from `ProcInfo` wire struct (serialized by procmgr's `proc_info_handler`), not raw procmgr reply. Parse ProcInfo via postcard.
  Must NOT do: NO `thread_enumerate` for PID listing (that's for TIDs — we want PIDs from procmgr). NO `call_with_reply_buf` or `call_with_reply_buf_timeout` (replaced by IpcCallFuture). NO keeping old `MountBackend` impl alongside `AsyncMountBackend` (one or the other). NO blocking IPC calls in the async methods.
  Parallelization: Wave 2 | Blocked by: T2 (AsyncMountBackend trait), T3 (procmgr PID API) | Blocks: T6 | Can parallelize with: T4
  References:
    - `userspace/vfs/src/procfs.rs:238-240` — ProcfsBackend struct (procmgr_endpoint: usize)
    - `userspace/vfs/src/procfs.rs:243` — ProcfsBackend::new
    - `userspace/vfs/src/procfs.rs:253-276` — query_procmgr (REMOVE — replaced by IpcCallFuture)
    - `userspace/vfs/src/procfs.rs:310-315` — query_tid_list (REMOVE — replaced by IpcCallFuture list_pids)
    - `userspace/vfs/src/procfs.rs:318-348` — current open() (rewrite as open_async)
    - `userspace/vfs/src/procfs.rs:350-414` — current readdir() (rewrite as readdir_async)
    - `userspace/vfs/src/procfs.rs:327` — static files list (meminfo, cpuinfo, uptime)
    - `userspace/vfs/src/procfs.rs:337` — parse_pid_path (reuse for PID extraction)
    - `userspace/session-procmgr/src/proc_query.rs:99-107` — stat format string: "pid (name) state cpu_ticks heap_pages other_pages ppid sid cid pcid"
    - `userspace/libs/procmgr-common/src/wire.rs:83-90` — ProcInfo wire struct (fields available for stat formatting)
    - `userspace/vfs/src/mount.rs:72-152` — existing MountBackend trait (for reference)
    - T1's `IpcCallFuture` API: `IpcCallFuture::new(endpoint, msg) -> Self`, `impl Future<Output = Result<(Message, usize)>>`
    - T3's label constants: `PROCMGR_LIST_PIDS_LABEL`, `PROCMGR_PROC_INFO_LABEL`
  Acceptance criteria: `cargo xtask build` succeeds. QEMU boots. `top` displays session processes (shell, micropython if spawned). `ls /proc` shows PID entries. `cat /proc/<pid>/stat` shows formatted stat line. **Critical: spawning micropython in second cluuterm while `top` is running does NOT hang** — the deadlock is broken.
  QA scenarios:
    - Happy: `cargo xtask run`, login, `top` → shows processes. Open second cluuterm (compositor), `spawn micropython -c "print(2**64)"` → completes. `top` in first terminal still responsive, shows micropython. Evidence: .omo/evidence/task-5-async-vfs-pid-proc-top-spawn.txt
    - Failure: `top` running, rapid `spawn micropython` + `exit` cycles → no hang, no panic. Evidence: .omo/evidence/task-5-async-vfs-pid-proc-rapid-spawn.txt
  Commit: Y | feat(procfs): PID-keyed async /proc via AsyncMountBackend + IpcCallFuture

- [ ] 6. Cleanup — revert timeout hack, KB notes, regression tests
  What to do:
    - **Revert timeout hack**: Remove `call_with_reply_buf_timeout` usage from `userspace/vfs/src/procfs.rs` (should already be gone after T5, but verify). Remove `call_with_reply_buf_timeout` function from `userspace/libcluu/src/ipc.rs:914-938` IF no other callers exist (grep for `call_with_reply_buf_timeout` across userspace/). If other callers exist, keep the function but add a doc comment "Deprecated: prefer async_runtime::IpcCallFuture".
    - **Verify no `call_with_reply_buf_timeout` calls remain in procfs.rs** — the old `query_procmgr` function should be deleted in T5.
    - **KB notes**: Write two knowledge base entries:
      1. `~/agentic-knowledge/patterns/cluu/cluu-async-runtime-no-std.md` — pattern note: single-threaded no_std async executor in libcluu, IpcCallFuture bridge (ipc_send + cookie correlation + dedicated reply endpoint), dual MountBackend/AsyncMountBackend trait pattern. Related: [[cluu-per-session-vfs-architecture]].
      2. `~/agentic-knowledge/decisions/cluu/cluu-pid-keyed-proc.md` — decision note: /proc rekeyed from TID to PID. Rationale: procmgr owns procs (PID), kernel owns threads (TID). VFS was doing TID→PID translation via blocking IPC (deadlock source). PID-keyed /proc queries procmgr directly, no translation. TID-keyed /proc stays for root-VFS admin view (deferred). Related: [[cluu-session-scoped-thread-enumeration]], [[cluu-per-session-vfs-architecture]].
      3. Update `~/agentic-knowledge/decisions/cluu/cluu-per-session-vfs-architecture.md` — add note about async VFS event loop resolving the session-VFS ↔ session-procmgr deadlock.
    - **KB commit**: `cd ~/agentic-knowledge && git add -A && git commit -m "kb: add async runtime pattern + PID-keyed /proc decision"`
    - **Regression tests**: Run `bash scripts/harness_suite.sh` and verify:
      - `l2_login` passes (may be flaky — pre-existing, run 3x)
      - `l2_jobs_basic` passes
      - `l2_cd` passes
      - `l2_ls` passes
      - `pm_proc_query_all_cap` passes
      - `l2_rm` passes
      - `l2_mkdir` passes
    - Record any NEW failures (not pre-existing ones listed in README)
  Must NOT do: NO deleting tests to make them pass. NO modifying test expectations. NO skipping the KB notes (they are a deliverable). NO marking done without regression evidence.
  Parallelization: Wave 3 | Blocked by: T4, T5 | Blocks: none | Can parallelize with: none
  References:
    - `userspace/vfs/src/procfs.rs:266` — `call_with_reply_buf_timeout(..., 100)` (the hack to remove)
    - `userspace/libcluu/src/ipc.rs:914-938` — `call_with_reply_buf_timeout` function definition
    - `scripts/harness_suite.sh` — full integration test suite
    - `scripts/harness_repeat.sh` — single-test repeat runner
    - `~/agentic-knowledge/decisions/cluu/cluu-per-session-vfs-architecture.md` — existing decision note to update
    - `~/agentic-knowledge/patterns/cluu/cluu-session-scoped-thread-enumeration.md` — related pattern note
    - `~/agentic-knowledge/_meta/AGENT-CONTRACT.md` — KB writing protocol
  Acceptance criteria: 
    - `rg "call_with_reply_buf_timeout" userspace/vfs/src/procfs.rs` → no matches
    - `cargo xtask build` succeeds
    - `cargo xtask run` boots, login works, top works, spawn works
    - `bash scripts/harness_suite.sh` — no NEW failures beyond README-listed pre-existing ones
    - KB notes exist at the specified paths
  QA scenarios:
    - Happy: Full regression suite passes. Evidence: .omo/evidence/task-6-async-vfs-pid-proc-regression.log
    - Failure: If a test fails, capture output, determine if pre-existing (document) or new (fix or report). Evidence: .omo/evidence/task-6-async-vfs-pid-proc-regression-failures.txt
  Commit: Y | chore: revert timeout hack, add KB notes, verify regression suite

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for the user's explicit okay before declaring complete.
- [ ] F1. Plan compliance audit — verify every todo was implemented as specified, no scope creep
- [ ] F2. Code quality review — `cargo clippy`, no `unsafe` except documented lifetime extension, no `as any`-equivalent
- [ ] F3. Real manual QA — QEMU boot + login + top + spawn micropython in second terminal + no hang
- [ ] F4. Scope fidelity — no kernel changes, no sync backend conversions, no session-procmgr async refactor

## Commit strategy
- One commit per todo (6 commits total)
- Commit messages follow conventional commits with scope
- All commits on `develop` branch
- Do NOT push — user will review and push manually

## Success criteria
1. `cargo xtask build` succeeds with no errors
2. QEMU boots, login works, shell works
3. `top` displays session processes (shell, and any spawned children)
4. **Spawning micropython in a second cluuterm while `top` is running does NOT hang** — the deadlock is broken
5. No `call_with_reply_buf_timeout` in procfs.rs
6. `libcluu::async_runtime` module exists with Runtime + IpcCallFuture
7. `AsyncMountBackend` trait exists in mount.rs
8. session-procmgr has `list_pids` + `proc_info` handlers
9. Regression suite: no new failures beyond pre-existing ones
10. KB notes written and committed
