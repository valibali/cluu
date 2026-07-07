# CLUU — Agent Operating Premises

> Permanent system prompt for any agent working in this repository.
> Read before touching code. These premises are non-negotiable unless a
> subsequent premise contradicts and is dated later.

## 1. What CLUU is

A hobby microkernel + minimal POSIX-flavored userspace in Rust (no_std),
seL4-inspired, pre-v1. Kernel knows three things: **threads**, **capability
tokens**, **IPC**. Everything else (processes, filesystem, scheduler policy,
network) lives in userspace services. See `doc/book/architecture.md` for the map
and `doc/book/kernel.md` for the long form.

## 2. Capability and visibility (the core invariant)

Every authority-bearing operation goes through a **capability token**
(HMAC-signed, kernel-verified, scope/rights/expiry-bound). Authority is
**declared at spawn time** via a `Cluufile` manifest (`containers/<name>/Cluufile`)
→ `manifest.toml` → procmgr applies the envelope. The kernel never inspects
Cluufiles; procmgr is the authority broker.

- **No new syscalls** for new userspace features — add an `InvokeOp` on the
  existing token-dispatch path. ~51 invoke ops today, ~12 syscalls total.
- **Visibility is capability-scoped.** A binary sees only what its envelope's
  VFS view + mount policy grants. `/proc`, `/etc`, `/dev`, `/tmp` access is
  the *result* of a cap-resolved view, not a filesystem fact.

## 3. No runtime ACL

There is **no runtime access-control list** layer. Authorization is decided
**once at spawn** (envelope construction + VFS `set_view` install) and then
enforced structurally by capability tokens + VFS view scoping. Do **not**
introduce per-call permission checks, policy engines, or "who is the caller"
interrogation at request time. If a binary can name a token, it can use it;
if it cannot, it never sees the endpoint. Add authority by **minting/revoking
tokens and shaping views**, not by adding ACL rules.

## 4. Containerization = encapsulation at spawn (NOT Docker)

A CLUU "container" is a **capability-scoped binary**, not an image bundle.
There is no parallel runtime, no namespace+cgroup recreation, no replicated
rootfs. The `containers/` directory name is historical. The precise model:

1. `Cluufile` declares `PROFILE` (cap bitmask), `MOUNT` policy per path,
   `ENTRYPOINT`, optional `PRELOAD`.
2. `container-build` emits `manifest.toml`.
3. procmgr reads the manifest at spawn, builds the envelope, calls
   `space_create` + `thread_create(START_SUSPENDED)`, sends `VFS_SET_VIEW`,
   then `thread_resume`. The suspend-bracket closes the view-install race.

Mount policy (`MOUNT /tmp inherit|private|readwrite|ro`) composes parent and
child views; `memfs_cid` pins MemFs ownership per mount.

## 5. Session encapsulation

A **session** is the unit of process ownership and view scoping. Each login
gets a `session-procmgr` (owns the session's children, exit cookies, signals,
pipes, process groups) and a `session-vfs` (owns the session's VFS view
layered on top of root-VFS's ext2/initrd backends). Children spawned inside a
session carry `PARAM_SESSION_VFS_EP` so `subscribe_output("vfs", "main")`
resolves to the **session-VFS**, not root-VFS — this redirection is in
`userspace/libcluu/src/registry.rs` and is the canonical way a session
binary reaches its session-VFS.

A session binary must only observe and affect processes **within its own
session**. Cross-session visibility is a privilege, not a default.

## 6. Root session has godmode

The **root session** is exempted from §5's session encapsulation: root's
session-procmgr/session-vfs may observe and affect processes across the
**whole system** (all sessions, all containers, kernel telemetry). This is
the *only* sanctioned escape hatch and it is bound to the root identity, not
to a capability that can be forwarded. Do not add a second godmode path.

## 7. Async runtime — canonical deadlock-avoidance mechanism

The async runtime in `libcluu::async_runtime` is the **canonical
deadlock-avoidance mechanism** for single-threaded servers. VFS and
session-procmgr use it. devmgr stays sync (leaf service, no downstream
IPC, no async runtime needed — async callers like VFS use `IpcCallFuture`
when querying devmgr). The sync `MountBackend` trait
remains for in-process backends (memfs, ext2-via-remote cached reads,
devfs null/zero/urandom) that never cross a process boundary.

**All IPC-bound VFS backends must use `AsyncMountBackend`** —
`ProcfsBackend` (→ procmgr), and the tty-read / PTS-verb dispatch paths
(→ tty driver, cluuterm). The async runtime (`Runtime`, `IpcCallFuture`,
`spawn`, completion queue) is wired into the VFS main loop at
`vfs/src/main.rs` and is the dispatch path for all `VfsOp` variants on
async mounts.

**History:** The original §7 forbade async for `top`/`/proc` and mandated
the sync `call_with_reply_buf` path. That constraint was based on the
pre-async-runtime state. The async runtime has since proven stable and
is the only structural fix for the single-threaded mutual-blocking IPC
deadlock class (see `doc/book/gotchas.md#cluu-single-threaded-mutual-blocking-ipc-deadlock`).
The sync-only constraint was lifted on 2026-07-06.

## 8. Debugging and verification

- **Harness** (`scripts/harness_run.sh`, `doc/book/testing.md`): boot CLUU in
  headless QEMU, inject keystrokes via monitor socket, validate serial
  markers on COM2. Use `TEST_COMMAND`, `MARKER_MODE`, `KEYSTROKE_COMMANDS`,
  `SENDKEY_SEQUENCE` for case control. Login creds in harness: `root`/`root`
  (sent via sendkey sequence `CREDS_SENDKEY_ROOT`).
- **GDB** (preferred for hangs): `QEMU_GDB=1 HARNESS_GDB_MODE=auto-continue`
  to resume a paused boot, or `cargo xtask run --debug` for an interactive
  `-S -s` session on `localhost:1234` with telnet serial on `:4321`. Kernel
  ELF at `target/x86_64-cluu-kernel/debug/deps/kernel-*.elf`; userspace
  ELFs at `target/x86_64-cluu-user/debug/<name>.elf`. See `doc/book/debugging.md`.
- **Evidence before assertions.** Reproduce the bug, capture the serial log
  and/or GDB backtrace, then propose a fix. Never claim "should work"
  without a verification run.

## 9. Change discipline

- Match the repo style: `no_std`, `alloc` explicit, `Result<T>` over
  panics, `debug_print` for serial diagnostics, no `as any`/`unwrap` in
  new code (CLuu is audited; rust-best-practices skill applies).
- Bug fixes are **minimal**: change what the bug requires, do not refactor.
- Respect §2–§6: no runtime ACL, no cross-session leaks, no authority
  outside the envelope, root godmode stays root-bound.
- Never commit, push, or alter shared state without explicit request.
