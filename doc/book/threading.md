# Threading

CLUU has a three-layer threading model: a kernel scheduler, userspace POSIX
threads (pthreads), and a single-threaded async runtime. Each layer is built
on the one below it — no layer duplicates functionality that belongs to a
lower layer.

```text
┌─────────────────────────────────────────────────────────────┐
│  Layer 3: Single-threaded async runtime (libcluu)           │
│  ─ IpcCallFuture, cookie correlation, completion queue       │
│  ─ For servers that need multiple in-flight IPC requests     │
│  ─ VFS, session-procmgr use this                             │
├─────────────────────────────────────────────────────────────┤
│  Layer 2: POSIX pthreads (libcluu::posix::pthread)          │
│  ─ pthread_create/join/detach, mutex, condvar, once, keys   │
│  ─ Built on ThreadCreate invoke op + kernel futex            │
│  ─ For C programs (newlib) and Rust programs needing threads │
│  ─ 64 KB stacks, TLS variant II, guard pages                 │
├─────────────────────────────────────────────────────────────┤
│  Layer 1: Kernel scheduler (kernel/src/sched/)              │
│  ─ O(1) priority bitmap, 256 levels, active/expired arrays  │
│  ─ Lazy FPU save/restore (FXSAVE/FXRSTOR)                   │
│  ─ APIC timer preemption at 250 Hz                           │
│  ─ ThreadCreate/Resume/Suspend/SetPriority invoke ops        │
│  ─ Fault forwarding via ThreadSetFaultEndpoint               │
└─────────────────────────────────────────────────────────────┘
```

## Layer 1: Kernel scheduler

Source: `kernel/src/sched/`

### Thread states

```rust
pub enum ThreadState {
    Init,    // Created, not yet ready
    Ready,   // Waiting for CPU
    Running, // On CPU now
    Blocked, // Waiting for IPC/event
    Dead,    // Terminated
}
```

Transitions: `Init → Ready` (resume), `Ready → Running` (schedule),
`Running → Ready` (preempt/expire), `Running → Blocked` (IPC wait),
`Blocked → Ready` (IPC wakeup), `* → Dead` (destroy).

A thread can also be `Suspended` via the `ThreadFlags::SUSPENDED` flag —
this is job-control (SIGSTOP/SIGCONT) and is orthogonal to `ThreadState`.
A suspended thread stays in its current state but is never picked by the
scheduler until unsuspended.

### Thread control block

`Thread` (`kernel/src/sched/thread.rs:182`) is `#[repr(C, align(64))]` —
64-byte cache-line aligned to avoid false sharing on the scheduler hot path.

Key fields:
- `id: ThreadId` — unique identifier
- `state: ThreadState` — current scheduling state
- `priority: Priority` — 0 (lowest) to 255 (highest), default 128
- `context: Context` — saved CPU registers (RAX–R15, RIP, RSP, RFLAGS, etc.)
- `fpu_state: FpuState` — 512-byte FXSAVE area for x87/SSE state
- `page_table_root: PhysAddr` — CR3 value for this thread's address space
- `time_slice_remaining: u64` — ticks left in current quantum
- `call_reply_info: Option<CallReplyInfo>` — pending IPC call reply target
- `fault_state: Option<FaultState>` — saved context when fault handler is active
- `session_id: u64` — visibility scoping (0 = system/root scope)
- `system_scope: bool` — privileged enumerate visibility

### Priority bitmap scheduler

`PriorityBitmapScheduler` (`kernel/src/sched/scheduler.rs`) is an O(1)
scheduler inspired by Linux 2.6:

- 256 priority levels (0 = lowest, 255 = highest)
- Two `PriorityArray` sets: **active** and **expired**
- Each array has a 256-bit bitmap + 256 FIFO queues (one per priority)
- `pick_next()`: find highest set bit in active bitmap → pop from that queue
- After a thread's time slice expires, it moves to the expired array
- When active is empty, swap active ↔ expired (pointer swap, O(1))

**Fairness guarantee**: Within one epoch (one complete swap cycle), every
thread — regardless of priority — gets exactly one time slice. Higher
priority threads run first within an epoch, but cannot starve lower priority
threads indefinitely.

`find_highest_priority()` scans 4 × 64-bit words from high to low, using
`leading_zeros()` to find the highest set bit in O(1).

### Preemption

The APIC timer fires at 250 Hz (set up in `kstart`). On each tick:

1. Current thread's `time_slice_remaining` decrements
2. If zero, the thread expires (moves to expired array, `pick_next` runs)
3. If a higher-priority thread became runnable (e.g., woken by IPC), the
   scheduler preempts immediately

Both cooperative and preemptive modes exist. In `INITMODE` (boot),
`ThreadFlags::COOPERATIVE` prevents preemption — threads yield explicitly.
In `NORMALMODE` (after boot), preemption is active.

### The non-preemptible-kernel invariant (single-CPU)

Kernel syscall and IRQ-handler code runs to completion without preemption.
The APIC timer IRQ checks the interrupted CPL: if CPL=3 (userspace), it
always reschedules; if CPL=0 (kernel) it only reschedules when the current
thread is idle (`interrupts.asm:661-668`, `idt.rs:1339-1347`). There is no
`preempt_disable` counter — non-preemptibility is structural, not counted.

This invariant is load-bearing for every check-then-block sequence in the
kernel:

- Futex `enqueue → block` (`handlers.rs:1993-2000`)
- Recv 3-tier arm/register/recheck (`handlers.rs:271-329`)
- Endpoint direct-deliver (`endpoint.rs:1022+`)
- `wake_thread` try_lock + `queue_pending_wake` fallback
  (`thread_manager.rs:941-972`)

If kernel preemption is ever introduced, every site listed above must be
audited. The `PerCpuReplyMap` `UnsafeCell<ReplyMap>` with `unsafe impl Sync`
(`thread_manager.rs:179-192`) is correct **only** under this invariant plus
the single-CPU assumption.

**SMP note:** SMP is a post-v1 (2027) possibility. This invariant dies with
SMP — every check-then-block site listed above needs a `preempt_disable`
section or a lock-ordering re-audit. The `cpu_id` field in `PerCpuData`
(`syscall.rs:84`) is a placeholder; no SMP abstraction is wired today.

### Context switch

The context switch is a two-step process:

1. **Save**: On schedule-out, `save_context()` (`thread_manager.rs:1466`)
   copies the CPU register state into `thread.context` and runs `FXSAVE`
   to save FPU/SSE state into `thread.fpu_state`. Validates RIP is in
   userspace range and canonical — halts on corruption.

2. **Restore**: On schedule-in, the assembly context-switch routine loads
   `thread.context` into CPU registers and runs `FXRSTOR` to restore
   `thread.fpu_state`.

FPU state is saved lazily in practice: the CR0.TS (Task Switched) bit is
set on context switch, and the first FPU instruction by the new thread
triggers a `#NM` fault that loads the thread's FPU state. This avoids the
512-byte FXSAVE on every switch for threads that don't use floating point.

### FPU state

`FpuState` (`kernel/src/sched/fpu.rs`): 512 bytes, 16-byte aligned
(`#[repr(C, align(16))]`). Initialized with:
- FCW = 0x037F (all exceptions masked, double precision, round-to-nearest)
- MXCSR = 0x1F80 (all SSE exceptions masked, round-to-nearest)

### Thread management invoke ops

| InvokeOp | Number | Purpose |
|---|---|---|
| ThreadCreate | 0 | Create thread in an address space (optionally suspended) |
| ThreadDestroy | 1 | Terminate a thread |
| ThreadSuspend | 2 | Suspend a thread (job control) |
| ThreadResume | 3 | Resume a suspended thread |
| ThreadSetPriority | 4 | Change scheduling priority |
| ThreadSetFaultEndpoint | 5 | Register IPC endpoint for fault notifications |
| ThreadSetFSBase | 6 | Set FS base (TLS pointer for pthreads) |
| ThreadGetId | 7 | Get thread's ID |
| ThreadGetStats | 8 | Get CPU ticks consumed |
| SchedGetOverflow | 9 | Get scheduler overflow diagnostics |

No new invoke ops were added for USB or threading work — the existing set
is sufficient.

### Fault forwarding

`InvokeOp::ThreadSetFaultEndpoint` (op 5) lets a thread register an IPC
endpoint as its fault handler. When any userspace fault occurs (page fault,
GPF, `#DE`, `#UD`), `try_forward_fault` (`idt.rs:425`) sends an IPC message
(label `0xFA017`) to the registered endpoint carrying `fault_type`,
`fault_addr`, `error_code`, `rip`, `thread_id`, `reply_id`. The faulting
thread is blocked and its full CPU + FPU context is saved in
`thread.fault_state`.

The handler replies via `reply_id` to resume the thread — optionally with
a modified context (the handler can rewrite GPRs/RIP/RSP). This is closer
to Mach exception ports / L4 IPC fault handlers than to POSIX signals.

procmgr uses this to track child crashes. The write-barrier GC pattern (for
interpreter porting) registers a fault endpoint on mutator threads,
`space_protect`s a page read-only, and on write fault the GC handler logs
the cross-generational pointer, unprotects the page, and replies to resume.

### Synchronization: kernel futex

The kernel provides a single synchronization primitive: `FutexWait` /
`FutexWake` (`kernel/src/sync/futex.rs`). A futex is a 32-bit aligned
userspace word. `FutexWait` puts the calling thread to sleep if the word
matches an expected value; `FutexWake` wakes waiting threads. This is the
foundation for userspace mutexes, condvars, and barriers.

## Layer 2: POSIX pthreads

Source: `userspace/libcluu/src/posix/pthread.rs`

### Thread creation flow

1. Parent allocates a stack via `SpaceMap` (64 KB, 16 pages) from the
   thread stack region `0x6000_0000..0x7000_0000`, with a guard page below
2. Parent allocates a TLS block on the heap (variant II layout)
3. Parent writes `PthreadStartup` on the child's stack
4. Parent calls `ThreadCreate` invoke op → gets child thread token
5. Parent stores token in `PthreadInternal` + TLS block (FS:8)
6. Parent calls `ThreadSetFSBase` on child token
7. Parent stores `ready=1`, `futex_wake`
8. Child trampoline waits for `ready`, then FS base is already correct

### TLS layout (x86_64 variant II)

```text
[.tdata copy][.tbss zeroed][padding][TCB: self-ptr(8), token(8), keys(64×8)]
                                      ^-- FS base points here
```

- FS:0 = TCB self-pointer (required by x86_64 ABI)
- FS:8 = thread token (custom CLUU slot, used by `pthread_self`)
- FS:16..FS:528 = `pthread_key` values (64 slots × 8 bytes)
- Negative offsets from FS = `__thread` variables

### Stack management

- Default stack: 64 KB (16 pages, `DEFAULT_STACK_PAGES = 16`)
- Stack region: `0x6000_0000..0x7000_0000` (256 MB)
- Guard page: 1 page below the stack, mapped as no-access. Stack overflow
  triggers a page fault on the guard page → thread killed or fault forwarded
- Bump allocator for stack addresses (`NEXT_STACK_ADDR` atomic)
- Detached thread reclamation: `pthread_entry` pushes a `ReapEntry` to
  `REAP_QUEUE` (can't free own stack while running on it). `pthread_create`
  drains the reap queue on next call.

### Synchronization primitives

All built on kernel futex:

- **Mutex** (`pthread_mutex_t`): 32-bit atomic with `UNLOCKED`/`LOCKED`/
  `CONTESTED` states. Uncontended lock is a single `cmpxchg`. Contended
  lock calls `FutexWait`; unlock calls `FutexWake`.
- **Condvar** (`pthread_cond_t`): futex-based wait/wake, paired with a mutex.
- **Once** (`pthread_once_t`): atomic init flag + futex for waiting.
- **Key/TSD**: 64 `pthread_key` slots with destructor functions, cleaned up
  on thread exit.

### Deferred-free interaction

The allocator's deferred-free queue (`allocator.rs`) interacts with pthreads
when a GC runs inside `malloc`/`free` and re-enters the allocator. See
[Gotchas: allocator-reentrancy-leak](gotchas.md#allocator-reentrancy-leak).

## Layer 3: Single-threaded async runtime

Source: `userspace/libcluu/src/async_runtime.rs`

### Purpose

A single-threaded server (like VFS or session-procmgr) that makes a
synchronous IPC `call` to another single-threaded server deadlocks if the
callee calls back into the caller. The async runtime is the canonical
deadlock-avoidance mechanism: it lets a single thread have multiple
outstanding IPC requests without blocking.

### Architecture

```text
┌──────────────────────────────────────────────────────┐
│                  Server main loop                     │
│                                                       │
│  1. rt.poll_ready()     — poll ready tasks           │
│  2. ipc_recv_any([server_ep, rt.reply_endpoint()])   │
│  3. if reply ep: rt.deliver_reply(cookie, msg, data) │
│  4. if server ep: handle request, rt.spawn(async{})  │
│  5. drain rt.pop_completion() for &mut self work     │
│  6. goto 1                                            │
└──────────────────────────────────────────────────────┘
```

The `Runtime` owns:
- `tasks: BTreeMap<TaskId, Task>` — boxed futures
- `ready_queue: VecDeque<TaskId>` — tasks to poll next
- `pending_cookies: BTreeMap<usize, TaskId>` — cookie → waiting task
- `replies: BTreeMap<usize, (Message, Vec<u8>)>` — cookie → reply data
- `completions: VecDeque<Box<dyn Any>>` — typed results from async tasks
- `reply_endpoint: usize` — dedicated IPC endpoint for replies

### IpcCallFuture

`IpcCallFuture` is a `Future` that sends an IPC request and awaits the reply:

1. **NotSent**: `ipc_send` to the target endpoint. Request carries
   `words[4] = reply_endpoint`, `words[5] = cookie`. On success → Waiting.
2. **Waiting**: Register cookie → task_id in `pending_cookies`. When the
   main loop receives a reply on `reply_endpoint`, it calls
   `deliver_reply(cookie, msg, payload)`, which stores the reply and
   re-queues the task. On next `poll_ready`, the task polls again,
   `take_reply(cookie)` succeeds → Done.
3. **Done**: Return `Ok((msg, payload))`.

Cookie correlation: each `IpcCallFuture::new` allocates a unique cookie
from the runtime. The reply echoes the cookie in `words[5]`, allowing the
runtime to match replies to waiting tasks.

### Waker

The runtime uses a noop waker — tasks are never woken by the waker
machanism. Instead, the runtime's `poll_ready` loop drives all ready tasks,
and `deliver_reply` re-queues tasks when their IPC reply arrives. This is
correct for the single-threaded model: the only thing that makes a task
ready is an IPC reply, and the main loop handles that explicitly.

### Completion queue

Async tasks can't hold `&mut self` references to server state (the server
struct outlives any single task, but the borrow checker can't prove this).
Instead, tasks push typed results to `completions: VecDeque<Box<dyn Any>>`
via `push_completion()`. The main loop drains these via
`pop_completion()` and does the `&mut self` work. This is how VFS
allocates fd table entries after an async open completes.

### Usage

VFS (`userspace/vfs/src/main.rs`), session-procmgr, and the shell
(`userspace/shell/src/main.rs`) use the async runtime. devmgr stays
sync (leaf service, no downstream IPC). The sync `MountBackend` trait
remains for in-process backends (memfs, ext2-via-remote cached reads,
devfs null/zero/urandom) that never cross a process boundary.

The shell adopted the async runtime on 2026-07-13, replacing a
pthread-based completion thread that lacked a VFS view. The shell's
main loop uses `ipc_recv_any` on `[completion_ep, reply_ep]`, spawns
async `readdir` tasks for cache warming, and reads stdin via
`read_grant_async` — all on a single thread with no locks held across
yield points.

## EHCI interrupt polling (no threading)

The `usb-input` service is single-threaded — it uses neither pthreads nor
the async runtime. Its main loop is a cooperative poll:

```rust
loop {
    // 1. Poll the interrupt IN qTD for HID report data
    if let Some(n) = ctrl.poll_interrupt(&report_dma, int_max_pkt) {
        // Process HID report...
        ctrl.setup_interrupt_in(&mut pool, addr, int_max_pkt, &report_dma);
    }

    // 2. Non-blocking IPC recv (timeout=0)
    match ipc_recv_any(&tokens, &mut buf, 0) {
        Ok((idx, len)) => { /* handle registry messages */ }
        Err(_) => { yield_cpu(); }  // No message → yield to scheduler
    }
}
```

The EHCI controller's interrupt endpoint is on the periodic schedule. The
HC automatically polls the device every frame (1 ms). When a HID report
arrives, the HC writes it to the qTD's DMA buffer and clears the ACTIVE
bit. The driver's `poll_interrupt()` checks the ACTIVE bit — if clear, it
reads the report data and re-arms the qTD.

`yield_cpu()` is `InvokeOp::Yield` (or `sys_yield`), which tells the
kernel scheduler to run other threads. Without it, the tight poll loop
would monopolize the CPU. With it, the scheduler time-slices fairly
between `usb-input` and other services.

This pattern — single-threaded poll + `yield_cpu` — is appropriate for
simple device drivers that don't need concurrent IPC. For servers that
need concurrent IPC (VFS, procmgr), the async runtime is the right choice.

## Comparison with other models

| Feature | Kernel scheduler | pthreads | Async runtime |
|---|---|---|---|
| Concurrency unit | Thread (kernel) | Thread (kernel) | Task (userspace future) |
| Preemption | Yes (APIC timer) | Yes (inherits) | No (cooperative) |
| Stack per unit | Yes (kernel-managed) | Yes (64 KB userspace) | No (shared stack) |
| IPC model | Blocking | Blocking | Non-blocking (cookie correlation) |
| Sync primitive | Futex | Mutex/condvar on futex | Noop waker + ready queue |
| Use case | All execution | C/Rust multi-threaded programs | Single-threaded servers |
