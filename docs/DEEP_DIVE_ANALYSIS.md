# CLUU Deep Dive Analysis — February 2026

**Scope**: Full kernel + userspace audit for general-purpose OS readiness
**Codebase**: ~42.6K LOC (kernel 18.3K, userspace 23K, klibcluu 1.3K)
**Goal**: Assess readiness for multi-TTY console OS, vim-like editor port, MicroPython w/threading

---

## 1. Executive Assessment

CLUU is in a **strong early-microkernel** state. The architecture is sound, the IPC is functional, the scheduler is well-designed, and the recent M0-M5 hardening work (sender authentication, leak diagnostics, fairness SLOs, CI harness matrix) puts it ahead of most hobby OS projects in terms of engineering rigor.

However, two structural gaps block the stated goals:

1. **No userspace threading primitives** (no pthreads, no futex, no TLS). This blocks MicroPython with threading and limits what can be ported.
2. **No select/poll/epoll equivalent**. This blocks any interactive editor (vim needs to multiplex terminal input with timers) and most real-world software.

These are not cosmetic gaps — they are load-bearing primitives that nearly all portable C software assumes.

---

## 2. Kernel Safety

### What's solid

- **Syscall entry**: SYSCALL/SYSRET with MSR setup. GS-based per-CPU data with compile-time layout verification. Stack swap is correct — user RSP saved to `gs:[0]`, kernel RSP loaded from `gs:[8]`, clean register state preservation.
- **IST usage**: 3 IST stacks (double-fault, GPF, page fault) prevent stack overflow from crashing the kernel.
- **Fault forwarding**: Userspace page faults/GPFs are forwarded via IPC to a fault handler endpoint with full register context and a reply token. This is the correct microkernel approach — no kernel policy decisions about faults.
- **Interrupt safety**: `DisableInterrupts` RAII guard pattern. IRQ-safe logger.
- **Memory isolation**: Each process gets its own page table. Kernel uses physmap for accessing physical memory. User pointers are validated via `validate_user_buffer()` + `copy_from_user()`.

### What's concerning

| Issue | Severity | Location |
|---|---|---|
| Global token handles are guessable sequential integers | HIGH | `token/table.rs:232` — `NEXT_HANDLE.fetch_add(1)` |
| SpaceDestroy is unimplemented — leaks page tables + frames | HIGH | `syscall/handlers.rs:824` — returns `NotImplemented` |
| Demand paging has no locking between concurrent page faults on same address | MEDIUM | `idt.rs` `handle_heap_fault()` calls `map_user_page()` without per-space lock |
| No TLB shootdown (single-CPU only assumed) | LOW (for now) | Not implemented anywhere |
| Single-entry per-thread token cache can thrash on multi-token hot paths | LOW | `thread.rs:211` — single `Option<TokenCacheEntry>` |

### Honest verdict on kernel safety

For **single-CPU, trusted userspace** (which is the current operating mode), safety is adequate. The kernel won't crash from userspace misbehavior — faults are forwarded or the thread is killed. The main systemic risk is the **global token namespace** allowing cross-process capability guessing. This matters when you add untrusted code.

**For the stated goal of porting software**: kernel safety is not the bottleneck. The bottleneck is missing userspace primitives.

---

## 3. Speed

### What's fast

- **Scheduler**: O(1) priority bitmap with active/expired arrays — identical algorithm to Linux 2.6. `pick_next()` is a 64-bit `find_highest_set_bit` + deque pop. Thread switch: save/restore 15 GPRs + swap CR3 + swap RSP. This is about as fast as it gets.

- **Token lookup (cache hit)**: Lock-free atomic generation check → existing shard lock → return cached. Skip HMAC verification entirely. The cache is single-entry but covers the common case (same endpoint token used repeatedly).

- **PMM alloc/free**: O(1) buddy allocator with intrusive free lists. Coalescing on free is O(log max_order) worst case but typically O(1). Allocation pops from free list head.

- **Timer**: 250Hz APIC timer = 4ms ticks. Reasonable for interactive use. TSC calibrated at boot via PIT.

### What's slow

| Hot path | Issue | Impact |
|---|---|---|
| Token lookup (cache miss) | Full HMAC-SHA256 verification | ~microseconds per miss. Noticeable under multi-token workloads |
| IPC message copy | Every send copies up to 4KB to kernel buffer, every recv copies back | 2 copies per message. For small messages (8 bytes), copying 4KB structures is wasteful |
| `sys_recv` multi-endpoint scan | Linear probe of up to 16 endpoints per receive | O(n) per recv call, mitigated by fair rotation hint |
| Scope resolution across shards | `resolve_scope()` iterates all 16 shards | O(16) lock acquisitions on miss |
| `revoke_tokens_for_object()` | Scans all tokens in all shards | O(total_tokens) — only matters during teardown |

### Speed verdict

For an interactive console OS with <10 processes, speed is not a concern. The IPC overhead is dominated by context switch cost (save/restore + TLB flush on CR3 swap), which is inherent to any microkernel. The current design has no performance cliffs — it degrades linearly, not catastrophically.

**For MicroPython**: The GC collect cycle scans the C stack (fast), and the REPL loop is I/O-bound on terminal input. Speed is not a bottleneck for the Python interpreter.

**For a vim-like editor**: Speed is fine. Terminal rendering is framebuffer-based, and the console service handles glyph rendering directly. The bottleneck will be feature completeness, not speed.

---

## 4. IPC System

### Architecture

```
Userspace A                    Kernel                     Userspace B
  ipc_send(ep, msg) ──→  sys_send() ──→ endpoint queue
                                                ↓
                          sys_recv() ←── dequeue + copy ──→ ipc_recv(ep, buf)
```

7 syscalls: `send`, `recv`, `call`, `reply`, `yield`, `invoke`, `debug_print`.

The `invoke` syscall is the workhorse — it dispatches 30+ operations (thread create/destroy/suspend/resume, space create/map/unmap/grant, endpoint create, IRQ attach/ack, PCI config, port I/O, clock, frame alloc/free).

### What works well

- **Call/reply RPC**: `sys_call` creates a one-time reply token, sends it with the message, blocks until reply arrives. Server receives message with reply token, processes it, calls `sys_reply`. This is clean and correct.
- **Multi-endpoint receive**: `sys_recv` takes up to 16 endpoint tokens and does a fair rotating scan. The `recv_wait_armed` flag closes the registration/block race.
- **Backpressure**: Queue has 1024-message cap. Senders block when full.
- **Sender authentication**: (WP-M4.1) Kernel injects authenticated sender thread ID into received messages. Services use this instead of trusting caller-supplied IDs.
- **Timeout support**: `block_current_with_timeout(deadline)` uses a min-heap for O(log m) timeout tracking.

### What's missing or fragile

1. **No zero-copy path**: Every message is copied twice (user→kernel→user). For large payloads (e.g., VFS read returning file data), this is wasteful. The `SpaceGrant` syscall exists for zero-copy page sharing but isn't wired into the IPC fast path.

2. **Fixed 4KB per queued message**: `EndpointMessage` stores `data: [u8; 4096]` inline. A queue of 1024 small (64-byte) messages wastes 3.9MB. Variable-size or slab-backed storage would help.

3. **No notification/signal channel**: There's no lightweight out-of-band notification mechanism (like seL4's Notifications). Everything goes through endpoint queues, even simple "wake up and check something" signals.

4. **IPC is the only blocking primitive**: `sleep()` is implemented by creating a dummy endpoint and doing a timed recv that always times out. This works but is architecturally inelegant.

### IPC verdict

The IPC is **functionally complete for a microkernel**. It supports all the patterns needed: one-shot messaging, RPC, multi-endpoint demuxing, timeouts. The sender authentication hardening is a genuine differentiator.

The main gap for porting software isn't IPC itself — it's the lack of a **multiplexing primitive** (select/poll) that lets a process wait on multiple I/O sources simultaneously.

---

## 5. Token / Capability System

### How it works

```
Token = {
    scope: OpaqueScope (16 bytes, random),
    rights: Rights (u64 bitmask),
    issuer: Kernel | Process(pid),
    expiry: Timestamp,
    signature: HMAC-SHA256(scope || rights || issuer || expiry, kernel_secret)
}

TokenHandle = monotonic u64 counter (global, not per-process)
```

Two-level mapping:
- `TokenHandle → Token` (handle table, sharded 16 ways)
- `OpaqueScope → ObjectRef` (scope table within each shard)

### What's strong

- **HMAC signatures**: Every token is signed. Tampering is detectable.
- **Subset derivation**: `invoke_token_derive` enforces `new_rights ⊆ parent_rights` and `new_expiry ≤ parent_expiry`. No privilege escalation through derivation.
- **Revocation generation**: Atomic counter incremented on any revoke. Thread caches check generation before reuse. This is a fast invalidation mechanism.
- **Object-wide revoke**: `revoke_tokens_for_object(ObjectRef)` removes all tokens referencing a specific thread/space/endpoint. Used during process cleanup.
- **Audit ring**: 256-entry circular buffer logging create/derive/revoke with rights and object info.

### What's weak

- **Global handle namespace**: Any process that can guess a valid handle can use it. Handles are sequential integers starting from 1, so handle `N+1` is trivially predictable after observing handle `N`. This is the single biggest architectural security issue. A per-process capability space (CSpace) would fix this.

- **Single-entry cache**: Each thread caches exactly one token. A service handling requests from multiple clients (different endpoint tokens) will thrash the cache on every context switch. A 4-entry LRU would capture most hot paths.

- **Scope resolution is cross-shard**: `resolve_scope()` iterates all 16 shards. This is called on every token lookup cache miss. For a system with thousands of tokens, this is 16 lock acquisitions per miss.

### Token system verdict

The rights model is good. The signature verification provides real integrity guarantees. The caching and revocation mechanisms are well-thought-out.

**The global handle namespace is the Achilles' heel**. For a trusted single-user system doing development work, it's acceptable — processes aren't actively trying to steal each other's capabilities. For a multi-user system with untrusted code, it would need to be replaced with per-process capability slots.

**For the stated goals** (vim port, MicroPython): the token system works fine. Processes receive tokens from their parent (procmgr), and as long as nobody is scanning for handles, access control holds.

---

## 6. Maturity Assessment

### Subsystem ratings

| Subsystem | LOC | Rating | Notes |
|---|---|---|---|
| Boot/init | ~800 | A | Clean, phased, properly ordered |
| x86_64 architecture | ~1.8K | A- | IST, APIC, TSC calibrated. Missing: SMP |
| Physical memory (PMM) | ~540 | B+ | Buddy allocator, 2-phase init, coalescing. Solid |
| Virtual memory (VMM) | ~1.7K | B | Demand paging works. Missing: concurrent fault lock, COW |
| Kernel heap | ~300 | A- | Proven `linked_list_allocator`, 16MB. May need growth |
| Scheduler | ~1.7K | A | O(1) bitmap, INIT→NORMAL transition, suspend/resume |
| IPC | ~2.5K | B+ | Queue-based, call/reply, sender auth, fairness scan |
| Token system | ~1.8K | B | Strong rights model. Weak: global handles, single cache |
| Syscall handlers | ~2.2K | B- | 7 syscalls + 30 invoke ops. Missing: select, futex, mmap |
| Telemetry | ~580 | A | Atomic counters, histogram, audit ring, harness modes |
| Procmgr | ~800 | B+ | Spawn, kill (SIGINT/TERM/KILL/STOP/CONT), exit, waitpid |
| VFS | ~800 | B- | Ext2 read+write, mkdir/rmdir/rename/unlink. Missing: mount/umount |
| Shell | ~1.2K | B | Builtins, spawn, bg/fg, jobs, Ctrl-C, command history |
| Console/TTY | ~1.4K | B | ANSI escape, SGR colors, UTF-8→CP437, raw mode |
| POSIX layer | ~2K | C+ | Basic file/process/time/termios. Many gaps remain |
| Harness/CI | ~400 | A- | 10+ marker modes, leak SLOs, churn tests |

### Overall maturity

CLUU is a **late-stage prototype / early-stage product** microkernel. The kernel is stable enough for daily development. The recent M0-M5 work on sender authentication, leak detection, and fairness SLOs shows genuine engineering discipline — most hobby kernels never reach this level of testing.

---

## 7. Correctness Audit

### What's correct

- Thread context save/restore: `Context` struct is `#[repr(C)]` with documented field order, matching the assembly in `syscall_entry.asm`.
- SysV ABI compliance: `context.rsp = stack.as_u64() - 8` for correct 16-byte stack alignment per x86-64 ABI.
- RFLAGS setup: `0x202` (IF set, reserved bit 1 set) — correct for Ring 3 threads.
- Segment selectors: CS=0x33 (user code, RPL 3), SS=0x2b (user data, RPL 3) — correct for SYSCALL/SYSRET.
- Buddy allocator coalescing: XOR buddy calculation (`frame ^ (1 << ord)`) is the standard correct approach.
- Timeout heap: Uses `BinaryHeap<Reverse<(u64, u64)>>` for min-heap semantics — correct.

### What needs attention

1. **`space_map` error paths**: If `map_user_page()` fails after allocating a frame, the frame IS freed in the error path (I verified this at `handlers.rs:993`). The rollback for `space_map_range` was hardened in WP-M3.1. This is now mostly correct.

2. **`copy_from_user` in `space_map`**: The handler now uses `userptr::copy_from_user()` with the caller's page table root (verified at `handlers.rs:951-953`). This is correct.

3. **`sys_recv` semantics**: The `recv_wait_armed` flag and the retry-on-WouldBlock pattern in userspace work correctly in practice (demonstrated by harness churn tests). The registration pass calls `recv_to_user` which registers the waiter as a side effect — architecturally awkward but functionally correct.

4. **Token scope cleanup**: When the last token with a given scope is removed from a shard, the scope→object mapping IS cleaned (`table.rs:96-103` checks if any remaining token shares the scope). This was flagged in the existing analysis but appears correct on re-examination.

---

## 8. What's Bleeding

### Active issues that affect daily use

1. **SpaceDestroy is unimplemented** — Every process exit leaks its page tables and mapped frames. Over many spawn/exit cycles, physical memory consumption grows monotonically. The harness already measures this (`delta_pmm_used_frames=45374` for 3 churn cycles). For sustained development sessions, this will eventually exhaust memory.

2. **VFS write path is new and undertested** — The ext2 write support (WP-L2.1) is recent. Creating files, appending, mkdir/rmdir work in harness tests, but the coverage matrix for edge cases (full disk, concurrent writes, partial block writes) is thin.

3. **Job control state transitions under heavy churn** — The WP-L2.2 notes say "remaining: stabilize broader job-state transition matrix under heavier churn." Stop/resume/fg/bg works for simple cases but may have edge cases with rapid state transitions.

---

## 9. What's Missing for the Goals

### Goal 1: General-purpose OS with multiple TTYs

| Missing piece | Effort | Description |
|---|---|---|
| **Virtual terminal multiplexer** | MEDIUM | Currently one TTY. Need VT switch (Alt+F1/F2) or a userspace tmux/screen-like multiplexer. Each VT needs independent terminal state, input routing, framebuffer region |
| **Per-TTY input routing** | MEDIUM | Keyboard service currently sends all input to a single endpoint. Need per-VT endpoint routing with a switchable foreground VT |
| **PTY (pseudo-terminal)** | HIGH | For running programs that expect `/dev/ttyN`. Need master/slave pair, ioctl support. Critical for ssh, screen, etc. |
| **Device file abstraction** | MEDIUM | No `/dev/` namespace. Programs can't open `/dev/tty`, `/dev/null`, etc. Need VFS device file support |
| **User/login system** | MEDIUM | No UID/GID. Need credential object in procmgr, login service, per-process credentials propagated through VFS |

### Goal 2: Port a vim-like editor

| Missing piece | Effort | Blocker level |
|---|---|---|
| **`select()` / `poll()`** | HIGH | **HARD BLOCKER** — vim/nano/kilo all need to wait on terminal input with timeout. No multiplexing primitive exists |
| **`fcntl()` with `O_NONBLOCK`** | LOW | Need non-blocking fd mode for terminal input polling |
| **`signal()` / `sigaction()`** | LOW | Need SIGINT (Ctrl-C) handler, SIGWINCH (terminal resize). Stubs exist in previous analysis but not yet implemented |
| **File write (ext2)** | DONE | WP-L2.1 landed write support |
| **Terminal raw mode** | DONE | `tcgetattr/tcsetattr` work, `TIOCGWINSZ` returns framebuffer size |
| **ANSI escape support** | DONE | Console handles CSI sequences, cursor movement, SGR colors |
| **UTF-8** | DONE | Multi-byte decoding with CP437 mapping |

The **hard blocker** is `select()`/`poll()`. Without it, no editor can work — they all need to read terminal input with timeout (for cursor blink, autosave, etc.). This requires:
- A kernel-level "wait on multiple events" primitive (not just multi-endpoint recv)
- OR: non-blocking reads + a sleep/timer mechanism
- OR: a dedicated `select` syscall that can wait on file descriptors

The simplest path: implement `poll()` as a wrapper that converts fd set → endpoint set + timeout, then uses `sys_recv` underneath. The challenge is that not all fds map to endpoints (file fds are VFS IPC, TTY fd is direct endpoint).

### Goal 3: Cross-compile MicroPython with threading

| Missing piece | Effort | Blocker level |
|---|---|---|
| **`pthread_create` / `pthread_join`** | HIGH | **HARD BLOCKER** — No userspace thread creation API. Kernel has `ThreadCreate` invoke, but no libpthread layer |
| **`pthread_mutex_*`** | HIGH | No futex or blocking mutex primitive. Spinlock only (unacceptable for userspace) |
| **`pthread_cond_*`** | HIGH | No condition variable support |
| **Thread-local storage (TLS)** | MEDIUM | No TLS (`__thread`, `_Thread_local`). MicroPython uses `mp_state_ctx` as thread-local |
| **`signal()` stub** | LOW | MicroPython needs it to not fail at link time. Previous session designed this |
| **`fcntl()` stub** | TRIVIAL | Same — link-time requirement |
| **`mmap` / `munmap`** | LOW | Basic exists. MicroPython uses `malloc` (via `sbrk`), not `mmap` |
| **REPL (no threading)** | DONE | All pieces exist for single-threaded MicroPython REPL |

For **MicroPython without threading**, the port is nearly ready — the previous session designed all port files (`mpconfigport.h`, `mphalport.c`, `main.c`, `Makefile`). Only `signal()` and `fcntl()` stubs remain.

For **MicroPython with threading**, the gap is enormous:
1. Need `pthread_create` → maps to kernel `ThreadCreate` + shared address space
2. Need `pthread_mutex` → needs a blocking primitive (futex)
3. Need TLS for per-thread state
4. Need `pthread_cond` for producer/consumer patterns

### Recommended implementation order for threading

```
1. futex syscall (kernel)           — the foundation for all blocking
2. pthread_mutex using futex        — most-used threading primitive
3. pthread_create/join              — thread lifecycle
4. pthread_cond using futex+mutex   — needed by many libraries
5. TLS via FS/GS segment base      — per-thread storage
```

The futex syscall is the single most important missing kernel primitive. It enables:
- pthread_mutex (contended case blocks via futex)
- pthread_cond (signal/broadcast via futex)
- semaphores
- read-write locks
- barriers

---

## 10. Token + User/Ownership Model

### Current state

CLUU uses capability-based access control through tokens. Every kernel object (thread, address space, endpoint, IRQ, frame) is accessed via a token handle carrying specific rights.

The VFS now tracks per-path ownership via authenticated sender ID (WP-L2.1). Procmgr enforces PID-owner authorization for kill (WP-M4.4). The registry enforces producer ownership on register/unregister (WP-M4.2).

### What's missing for a user system

1. **Credential object**: A kernel-backed or procmgr-backed credential that represents `(uid, gid, groups)`. Created at login, propagated to child processes via procmgr spawn.

2. **Capability transfer protocol**: Currently, procmgr passes tokens to child processes by writing them to a ProcessInfo page. There's no explicit kernel-mediated capability transfer (mint/grant/delegate). This is fine for single-user, but multi-user needs explicit delegation with audit trails.

3. **Per-process capability space**: Replace global handle table with per-space capability slots. This eliminates the handle-guessing attack vector entirely. Each process has a finite capability table, and handles are indices into that table.

### Recommended path

For the **near-term** (porting software, single-user): The current model works. Token handles are passed from procmgr to child processes. Services authenticate callers via kernel-injected sender IDs. This provides sufficient isolation for development.

For **multi-user**: Add a credential object to procmgr's spawn path. When spawning a process, the credential is bound to the process. VFS and other services check credentials for access decisions. This can be done entirely in userspace — the kernel doesn't need to know about UIDs.

The per-process capability space is a deeper architectural change. It's the right long-term direction but can be deferred until untrusted code actually needs to run.

---

## 11. Architecture Opinions

### What I'd prioritize (in order)

1. **`poll()` / event multiplexing** — Unblocks editor ports and most real-world software. Can be implemented as a thin layer over multi-endpoint `sys_recv` with a userspace fd-to-endpoint translation table.

2. **`futex` syscall** — Unblocks pthreads, which unblocks MicroPython threading and most C libraries. The kernel primitive is small (~100 lines: a hash table of wait queues keyed by virtual address).

3. **`SpaceDestroy` + process teardown** — Fixes the memory leak that will bite during sustained development sessions. Procmgr needs to: kill threads → revoke tokens → unmap pages → destroy space → free page tables.

4. **`signal()` / `fcntl()` stubs** — Trivial effort, unblocks MicroPython (single-threaded) and many other C programs at link time.

5. **pthreads userspace library** — Built on top of futex + ThreadCreate. Enables MicroPython threading.

6. **Multiple TTYs** — Virtual terminal switching. The console service already has all the rendering machinery; it just needs to be multiplied.

### What I'd skip for now

- **Per-process CSpace**: Correct long-term, but heavy refactor. The global handle table works for trusted development.
- **SMP support**: Single-CPU is fine for the current use case. SMP adds complexity everywhere (TLB shootdown, lock ordering, per-CPU scheduler queues).
- **Network stack**: Not needed for console-first OS development.
- **Dynamic linking**: Static linking works. Dynamic linking is a rabbit hole.
- **Formal verification**: The codebase is moving too fast for formal methods to add value.

---

## 12. Concrete Next Steps

Execution-tracked version of this section: `docs/archive/plans/deep-dive-implementation-plan.md` (IPC optimization first, then Phases A-D).

### Phase A: Unblock software porting (poll + signal + fcntl)

```
Kernel:
  - No kernel changes needed for poll() — build on sys_recv

Userspace (libcluu):
  - Add signal.rs: signal(), raise(), sigaction() stubs
  - Add fcntl() to file.rs: F_GETFL, F_SETFL, F_DUPFD
  - Add poll.rs: poll() wrapping multi-endpoint sys_recv
    - fdset tracks fd → endpoint mapping
    - POLLIN on TTY fd → recv on TTY endpoint
    - POLLIN on file fd → always ready (VFS does blocking IPC)
    - Timeout → sys_recv timeout

  Build MicroPython (single-threaded REPL)
  Port kilo/micro editor (minimal vim-like, ~1000 LOC C)
```

### Phase B: Fix resource leaks (SpaceDestroy + teardown)

```
Kernel:
  - Implement invoke_space_destroy: walk page tables, free frames, free PT pages
  - Add space reference counting (threads using this space)

Userspace (procmgr):
  - On child exit: thread_destroy → revoke_tokens_for_object → space_destroy
  - Track per-process resources for accounting
```

### Phase C: Enable threading (futex + pthreads)

```
Kernel:
  - Add sys_futex: FUTEX_WAIT (block if *addr == val), FUTEX_WAKE (wake N waiters)
  - Hash table of wait queues, keyed by (space_id, virtual_address)

Userspace (libcluu):
  - Add pthread.rs: pthread_create (→ ThreadCreate with shared space),
    pthread_join (→ futex wait on exit flag), pthread_mutex_* (→ futex),
    pthread_cond_* (→ futex)
  - Add TLS: set FS base per thread via ThreadCreate args

  Build MicroPython with MICROPY_PY_THREAD=1
```

### Phase D: Multiple TTYs

```
Userspace:
  - Extend console service: N virtual terminals, each with independent state
  - Add VT switch handler (Alt+F1..F6 or similar)
  - Add keyboard routing: foreground VT gets input
  - Add /dev/ttyN device file support in VFS
```

---

## 13. Summary Table

| Question | Answer |
|---|---|
| Is the kernel safe? | Yes, for single-CPU trusted userspace. The main risk is global token handles. |
| Is it fast? | Yes. O(1) scheduler, O(1) PMM, cached token lookups. IPC has inherent copy overhead but it's not a bottleneck. |
| Is the IPC sound? | Yes. Call/reply, multi-endpoint, sender auth, backpressure, timeouts. Missing: notifications, zero-copy. |
| Is the token system correct? | Rights model is correct. Signatures prevent tampering. Handle namespace is the weakness. |
| What's bleeding? | SpaceDestroy leak, job control edge cases, VFS write coverage. |
| Can I port a vim-like editor? | **Not yet** — need `poll()` (or `select()`), `signal()`, `fcntl()`. |
| Can I cross-compile MicroPython? | **Single-threaded: almost** — need `signal()` + `fcntl()` stubs. **With threading: no** — need futex + pthreads. |
| What about multiple TTYs? | Architecture supports it. Console service needs to be multiplexed. Medium effort. |
| What about a user system? | Token system + sender auth provides the foundation. Need credential objects in procmgr for multi-user. |
| Biggest single gap? | `poll()` / event multiplexing. It blocks more software than any other missing feature. |
