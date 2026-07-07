# Design Philosophy

CLUU's design is shaped by a small set of invariants. Every subsystem is a
consequence of these. When the code and a doc page disagree, the code is
correct.

## 1. The kernel knows three things

The kernel knows **threads**, **capability tokens**, and **IPC**. Everything
else — processes, filesystems, scheduler policy, network, device drivers,
terminal stack, window system — lives in userspace services.

This is not "microkernel" as a marketing label. The kernel genuinely does not
have a process concept (`sched::process` and `sched::process_manager` were
retired in favor of a unified thread model). It does not have a filesystem. It
does not have a network stack. It does not have a device driver framework. It
has threads (the scheduler), capability tokens (the authority primitive), and
IPC (the communication primitive). Everything else is composed in userspace.

The practical consequence: the kernel is small (~50K LOC including architecture
code), the syscall surface is tiny (7 syscalls), and new userspace features
almost never require kernel changes.

## 2. Authority is structural, not conventional

This is the single most important design principle. **Authority is decided once
at spawn time and enforced structurally for the lifetime of the process.** There
is no runtime access-control list. There is no per-call identity check. There
is no "who is the caller" interrogation at request time.

### How it works

1. A binary's authority is **declared at spawn time** via a `Cluufile` manifest
   (`containers/<name>/Cluufile`) → `manifest.toml` → procmgr applies the
   envelope.
2. The envelope includes: a capability profile (bitmask of rights), a VFS view
   (list of mount paths + read/write rights), and a mount policy.
3. The kernel enforces authority through **capability tokens** — HMAC-signed,
   scope-bound, rights-bound, expiry-bound. The kernel verifies the signature
   on every operation. Possession of a valid token *is* authority.
4. The VFS enforces visibility through **view scoping** — a process sees only
   the paths its view grants. `/proc`, `/etc`, `/dev`, `/tmp` access is the
   *result* of a cap-resolved view, not a filesystem fact.

### What this means in practice

- If a binary can name a token, it can use it. If it cannot, it never sees the
  endpoint.
- If a binary's view doesn't include `/etc/passwd`, it cannot open `/etc/passwd`
  — not because a policy engine says no at open time, but because the path
  doesn't exist in its filesystem namespace.
- Adding authority means **minting or revoking tokens and shaping views**, not
  adding ACL rules.
- "What can X do?" is answered by reading the static envelope and view, not by
  running code.

### What this forbids

- No per-call permission checks. No `if caller_tid == allowed_tid { ... }`.
- No policy engines. No "who is the caller" at request time.
- No runtime identity resolution. No `resolve_caller_session()` in the IPC path.
- No ACL that can be widened at runtime without a cap derivation.

## 3. No new syscalls for new features

The syscall surface is fixed at 7: `Send(0)`, `Recv(1)`, `Call(2)`, `Reply(3)`,
`Yield(4)`, `Invoke(5)`, `DebugPrint(255)`.

New userspace features add an `InvokeOp` on the existing `Invoke` dispatch path.
There are 52 invoke ops today:

| Group | Ops | Range |
|-------|-----|-------|
| Thread | Create, Destroy, Suspend, Resume, SetPriority, SetFaultEndpoint, SetFSBase, GetId, GetStats | 0–9 |
| Space | Create, Destroy, Map, Unmap, Grant, MapRange, Protect, GetStats | 10–19 |
| Futex | Wait, Wake | 17–18 |
| Token | Derive, Revoke, GetInfo, DeriveScoped | 20–23 |
| IRQ | Attach, Ack | 30–31 |
| Endpoint | Create, Peek | 40–41 |
| PCI | ConfigRead, ConfigWrite | 50–51 |
| I/O Port | In8/16/32, Out8/16/32 | 52–57 |
| Memory | VirtToPhys, PmmAllocLarge, PmmGetStats | 58–62 |
| Clock | Now, Frequency | 60–61 |
| Frame | Allocate, Free, GetPhys | 70–72 |
| Notification | Create, Signal, Wait, Poll | 80–83 |
| Thread enum | Enumerate, SetSession, SetSystemScope | 84–86 |

This keeps the kernel attack surface bounded. The `Invoke` path validates a
token, checks rights, and dispatches to the handler. Adding a new operation is
adding a variant to `InvokeOp` and a handler in `syscall::handlers` — no new
syscall number, no new entry point.

## 4. Monotone-narrowing authority derivation

When a process spawns a child, the child's authority is **always a strict
subset** of the parent's. This is enforced structurally at two layers:

### Token layer

`Token::derive` refuses to escalate rights or extend expiration. A derived
token has ≤ rights and ≤ expiry than its parent. The token table enforces this
before installing the derived token.

### VFS view layer

`VfsViewTable::verify_monotone` checks that every child mount is a
narrower-or-equal subset of some parent mount: same or more-specific path
prefix, rights ≤ parent's. A child that asks for `rw` on a path the parent only
has `ro` is denied at spawn.

### Why this matters

Authority can only shrink as you descend the spawn tree. A compromised child
cannot escalate. A buggy child cannot widen its own view. The spawn tree is a
monotone-narrowing authority tree, and the narrowing is enforced by the
structure of the system, not by a policy that could regress.

## 5. Encapsulation at spawn (the "container" model)

A CLUU "container" is a **capability-scoped binary**, not a Docker image. There
is no parallel runtime, no namespace+cgroup recreation, no replicated rootfs.
The `containers/` directory name is historical; the precise model is:

1. `Cluufile` declares `PROFILE` (cap bitmask), `MOUNT` policy per path,
   `ENTRYPOINT`, optional `PRELOAD`.
2. `container-build` emits `manifest.toml`.
3. procmgr reads the manifest at spawn, builds the envelope, calls
   `space_create` + `thread_create(START_SUSPENDED)`, sends `VFS_SET_VIEW`,
   then `thread_resume`.

The suspend-bracket (step 3) closes the view-install race: the child thread is
created suspended, the view is installed, and only then is the child resumed.
Without this, the child would see the parent's filesystem namespace — an
authority leak.

See [Container Encapsulation](../containers/index.html) for the full model.

## 6. Session encapsulation

A **session** is the unit of process ownership and view scoping. Each login
gets:

- A **session-procmgr** — owns the session's children, exit cookies, signals,
  pipes, process groups.
- A **session-vfs** — owns the session's VFS view layered on top of root-VFS
  backends.

A session binary must only observe and affect processes **within its own
session**. Cross-session visibility is a privilege, not a default.

The **root session** is the sole exception: root's session-procmgr may observe
and affect processes across the **whole system** (all sessions, all containers,
kernel telemetry). This is the only sanctioned escape hatch, and it is bound to
the root identity, not to a capability that can be forwarded. Do not add a
second godmode path.

See [Session Encapsulation](../sessions/index.html) for the full model.

## 7. Async runtime as deadlock-avoidance

VFS and session-procmgr are single-threaded servers. A naive synchronous IPC
pattern produces a classic deadlock: VFS calls procmgr (to resolve `/proc`),
procmgr calls VFS (to install a view), both block forever.

The async runtime in `libcluu::async_runtime` is the **canonical
deadlock-avoidance mechanism**. VFS dispatches IPC-bound backend operations
through `dispatch_async()`, so a single-threaded server can have multiple
outstanding downstream calls without blocking itself. The sync `MountBackend`
trait remains for in-process backends (memfs, ext2 cached reads, devfs) that
never cross a process boundary.

`devmgr` stays sync — it is a leaf service with no downstream IPC, so the async
runtime is not needed there. The rule is: if a server makes IPC calls to
another server that might call back into it, use the async runtime.

## 8. Evidence before assertions

CLUU's development discipline: reproduce the bug, capture the serial log and/or
GDB backtrace, then propose a fix. Never claim "should work" without a
verification run. The harness (`scripts/harness_run.sh`, Python
`cluu_harness/`) boots CLUU in headless QEMU, injects keystrokes via the
monitor socket, and validates serial markers on COM2.

## 9. Code discipline

- `no_std`, `alloc` explicit, `Result<T>` over panics.
- `debug_print` for serial diagnostics.
- No `as any`/`unwrap` in new code.
- Bug fixes are **minimal**: change what the bug requires, do not refactor.
- Respect the invariants above: no runtime ACL, no cross-session leaks, no
  authority outside the envelope, root godmode stays root-bound.
