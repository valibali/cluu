# The Kernel

The CLUU kernel is a `no_std` Rust binary targeting `x86_64-cluu-kernel`. It
provides three primitives — threads, capability tokens, and IPC — plus the
minimal syscall surface to use them. Everything else is userspace.

## Kernel entry (`kernel/src/main.rs`)

`_start` is a naked assembly entry point. It reads the APIC ID via CPUID, parks
non-BSP cores in a `hlt` loop (SMP bring-up is not implemented), switches the
BSP to a 64 KiB aligned kernel stack, and jumps to `kstart`.

`kstart` is the Rust kernel entry. It runs the full init sequence in a
load-bearing order:

```text
UART → logger → GDT → PIC → IDT → PS/2 aux → SMAP/SMEP → Spectre V2 →
syscall MSRs + per-CPU data → IPC fast-path toggles →
MM init → heap init → frame_table init → crypto/token init →
TSC calibration → APIC timer (250 Hz) → bootstrap::init → ThreadManager::start
```

After `ThreadManager::start` returns, the BSP falls through to `idle_loop` — a
`hlt` loop that sleeps until the next interrupt.

The kernel also installs the `#[panic_handler]`, which disables interrupts,
logs the panic location and message, walks the RBP-chained stack frames (up to
32 frames, bounded by `rsp + 256 KiB`), and halts.

## Subsystem overview

### Architecture (`kernel/src/architecture/`)

Architecture dispatch shim. Today only `x86_64` is wired in. Portable subsystems
call `architecture::x86_64::*` for CPU-specific setup; nothing under
`architecture/` calls back into portable code.

Sub-modules:
- **`gdt`** — GDT (kernel/user segments, TSS).
- **`idt`** — IDT (exception handlers, interrupt handlers).
- **`pic`** — 8259 PIC.
- **`apic`** — Local APIC timer + x2APIC.
- **`syscall`** — SYSCALL/SYSRET MSR setup, per-CPU data.
- **`interrupts`** — Interrupt entry/exit, IRQ dispatch.
- **`spectre`** — Spectre V2 mitigations.
- **`tsc`** — TSC calibration.
- **`abi_check`** — SysV-ABI check at boot.

Boot ordering is load-bearing: `gdt::init` → `pic::init` → `idt::init` →
`syscall::init`.

### Memory management (`kernel/src/mm/`)

Owns every frame of memory the kernel can allocate or map.

- **`pmm`** — Physical memory manager. Buddy allocator with bitmap + intrusive
  free lists.
- **`vmm`** — Virtual memory manager. `PageTableManager`,
  `create_initial_page_tables`, CR3 switch.
- **`physmap`** — Direct physical mapping. The only window onto physical memory
  after `init` switches off bootloader page tables.
- **`heap`** — Kernel heap. `linked_list_allocator` backed by PMM.
- **`space`** — `AddressSpace`, `MemoryRegion`, `layout`.
- **`space_repository`** — `AddressSpaceId` → `AddressSpace` table.
- **`frame_registry`** — Token-keyed frame ownership (advisory, Phase 1).
- **`frame_table`** — Per-frame ownership (advisory, Phase 1).
- **`boot`** — `BootInfoProvider` trait (bootloader-agnostic).
- **`pat`** — PAT MSR programming (index 1 = write-combining).
- **`user_map`** — Userspace mapping helpers.
- **`mock`** — `MockPageAllocator` for tests.
- **`traits`** — `PageAllocator`, `VirtualMemoryMapper`, `PageFaultHandler`.

`mm::init` is called once from `kstart`. After it returns, the bootloader's
page tables are gone; the kernel runs on its own PML4 with `physmap` as the
only window onto physical memory.

`allocate_user_stack` is a bootstrap helper that allocates and maps a
downward-growing userspace stack, tagging pages with `KERNEL_OWNER`.

### Scheduler (`kernel/src/sched/`)

- **`thread`** — `Thread`, `ThreadId`, `ThreadState`, `Priority`,
  `CallReplyInfo`, `FaultState`, `FaultType`, `ThreadFlags`.
- **`thread_manager`** — `ThreadManager` singleton. `SchedulerMode`
  (`INITMODE`/`NORMALMODE`).
- **`scheduler`** — `PriorityBitmapScheduler`. O(1) per pick via priority
  bitmap. `SchedulingPolicy` trait.
- **`context`** — `Context` for context switch.
- **`repository`** — `ThreadRepository` (storage).
- **`fpu`** — Lazy FPU state. Tracked separately from `Context` because the FPU
  is restored lazily.

The scheduler is O(1) per pick via a priority bitmap; `Priority` is the bit
index. Preemption is driven by the APIC timer tick set up in `kstart` (250 Hz).

There are 256 priority levels. Threads at the same priority run FIFO: the
scheduler does not reorder within a level. Both cooperative and preemptive
modes exist. Policy (who runs next, at what priority) is separated from
mechanism (the context switch, the bitmap scan), so a userspace scheduler
policy could later replace the in-kernel one without touching the dispatch
path.

`process`, `process_manager`, and `spawn` modules were retired on 2026-05-18 in
favor of a unified thread model. The file paths are kept as empty stubs but the
`mod` declarations are gone so the compiler rejects stale imports.

### IPC (`kernel/src/ipc/`)

Synchronous rendezvous-based message passing.

- **`endpoint`** — `Endpoint` state, fast-path toggles
  (`set_rendezvous_direct_enabled`, `set_register_fast_enabled`).
- **`message`** — `Message`, `MessageTag`, `BufferDesc`, `IpcFlags`, `IpcOp`.
- **`rendezvous`** — `RendezvousPoint`.
- **`transfer`** — `BufferTransfer` (Copy, Grant, Map).
- **`notification`** — `Notification` objects.
- **`traits`** — `IpcEndpoint`, `MessageTransfer`.

Key invariants:
- Synchronous and rendezvous-based: sender blocks until receiver ready and vice
  versa. No buffered queue.
- Authority to target an endpoint is proved by presenting a capability token.
- Buffer transfer: `Copy` (safe but slow), `Grant` (zero-copy page transfer),
  `Map` (zero-copy shared mapping).
- Fast path: 6 register-passed words without touching memory.
- Fast-path toggles are enabled unconditionally at boot by `kstart`.

### Syscall (`kernel/src/syscall/`)

- **`handlers`** — `sys_send`, `sys_recv`, `sys_call`, `sys_reply`,
  `sys_yield`, `sys_invoke`, `sys_debug_print`.
- **`userptr`** — Userspace pointer validation.

`SyscallNumber` enum: `Send(0)`, `Recv(1)`, `Call(2)`, `Reply(3)`, `Yield(4)`,
`Invoke(5)`, `DebugPrint(255)`. `from_usize` returns `None` for unknown numbers
so an invalid syscall surfaces as a clean error, not a panic.

Every handler validates token handles, checks expiration and signature,
verifies rights, and bounds user pointers to the userspace range before acting.
Handlers return `Result` and never panic.

### Capability tokens (`kernel/src/token/`)

See [Capability Tokens](../capability_tokens/index.html).

- **`rights`** — `Rights` bitmask.
- **`scope`** — `OpaqueScope`, `ObjectRef`, `AddressSpaceId`, `EndpointId`,
  `FrameId`, `NotificationId`, `ReplyId`.
- **`signature`** — `Signature`, HMAC-SHA256, `constant_time_eq`.
- **`table`** — Token storage, `kernel_secret`, `create_token`,
  `try_create_derived_token`, `revoke_token`, `resolve_scope`.

### Devices (`kernel/src/devices/`)

- **`irq`** — IRQ vector dispatch to userspace endpoints.
- **`ps2`** — PS/2 controller init for mouse aux port (one-shot, before
  userspace mouse driver attaches).

### Sync (`kernel/src/sync/`)

- **`futex`** — Kernel futex table. `FutexWait`/`FutexWake`. The single kernel
  synchronization primitive. Userspace POSIX threads (`libcluu::posix::pthread`)
  are built on this.

Pthread stacks are 64 KB (16 pages, `DEFAULT_STACK_PAGES = 16` at
`userspace/libcluu/src/posix/pthread.rs:108`), allocated from
`0x6000_0000..0x7000_0000` (`THREAD_STACK_REGION_START/END` at
`pthread.rs:113-114`), WITH a guard page below (`alloc_thread_stack` at
`pthread.rs:213-235`). The main process stack (mapped by procmgr via
`map_stack`) is 64 KB without a guard until the C1 upgrade lands.

### Telemetry (`kernel/src/telemetry.rs`)

Boot timing and diagnostics.

### Bootboot (`kernel/src/bootboot.rs`)

Bootboot protocol parsing.

### Bootstrap (`kernel/src/bootstrap/`)

Early boot assembly + init thread construction.

### ELF (`kernel/src/elf.rs`)

ELF loading helpers.

### Error (`kernel/src/error.rs`)

Kernel `Error` type.

### Interrupts & exceptions (`kernel/src/architecture/x86_64/idt.rs`)

A full x86_64 IDT with a handler for every CPU exception. Page faults are
integrated with the VMM: a fault on a lazily-mapped heap or stack page is
resolved by `handle_heap_fault`, a fault on a guard page kills the thread (or
forwards to a registered fault endpoint), and a fault with no resolution path
terminates the faulting thread cleanly.

The APIC timer IRQ drives preemption. On each tick the scheduler re-runs
`pick_next` and context-switches if a higher-priority thread became runnable.

Logging inside IRQ context is IRQ-safe: no locks, no allocation, manual
formatting. See [Logging](#logging) below.

### Logging

Kernel logging is a diagnostic tool, not a runtime dependency. It is
zero-cost in release builds, IRQ-safe, allocation-free, and UART-backed.
Formatting is manual (no `format!` machinery) so a log line never touches the
heap or takes a lock that an IRQ handler could deadlock on.

Output goes to the UART initialized first in `kstart`, before any other
subsystem, so early-boot and panic messages always have a path out.

## Subsystem status

Every kernel subsystem listed in the subsystem overview above is complete:
PMM, VMM, address spaces, scheduler, IPC, capability tokens, IRQ handling,
syscall infrastructure, IRQ-safe logging. The userspace ABI is stable. See
[Audit](../audit/index.html) for the per-subsystem audit findings and
[Roadmap](../roadmap/index.html) for what is still in progress on the
userspace side.

## Fault forwarding (ThreadSetFaultEndpoint)

`InvokeOp::ThreadSetFaultEndpoint` (op 5, `kernel/src/token/mod.rs:423`) lets a
thread register an IPC endpoint as its fault handler.

When any userspace fault occurs (PF, GPF, `#DE`, `#UD`), `try_forward_fault`
(`kernel/src/architecture/x86_64/idt.rs:425-520`) fires: it sends an IPC message
(label `0xFA017`) to the registered endpoint carrying `fault_type`,
`fault_addr`, `error_code`, `rip`, `thread_id`, `reply_id` (six words,
`idt.rs:457-466`). The faulting thread is blocked (`t.make_blocked()`,
`idt.rs:515`) and its full CPU + FPU context is saved in `thread.fault_state`
(`idt.rs:502-514`).

The handler replies via `reply_id` to resume the thread — optionally with a
modified context (the handler can rewrite GPRs/RIP/RSP).

If `try_forward_fault` fails (lock contention on the endpoint,
`idt.rs:479-482`), `queue_deferred_fault`
(`kernel/src/sched/thread_manager.rs:1059-1148`) defers the notification to the
next timer tick, where `drain_deferred_faults()` replays it.

procmgr uses this today (`userspace/root-procmgr/src/main.rs:1212` creates the
fault endpoint, `:4810` registers it per spawned thread) to track child
crashes.

This is closer to Mach exception ports / L4 IPC fault handlers than to POSIX
signals. It is the canonical primitive for write-barrier GC: register a fault
endpoint on mutator threads, `space_protect` a page read-only, and on write
fault the GC handler logs the cross-generational pointer, unprotects the page,
and replies to resume the mutator. See [Interpreter Porting](../interpreter_porting/index.html)
for the porting pattern.

## Documentation findings

Audit-trail items from the per-file documentation pass. Each has a stable
`F-NNN` ID; the source site carries a `TODO(doc-finding)` marker.

### F-001 — Stale boot-sequence TODO in kernel main.rs header (resolved)

The old `kernel/src/main.rs:11` header listed step 6 as "Enter idle loop (TODO:
start the scheduler and init process)". `kstart()` does both: `bootstrap::init`
creates the init thread (Phase 5) and `ThreadManager::start()` starts the
scheduler (Phase 8). The TODO was satisfied; only the comment was stale. The
header was rewritten in the documentation pass. See [Boot Flow](../boot/index.html)
for the current sequence.

### F-002 — Duplicate stale boot-sequence comment in architecture/mod.rs (resolved)

`kernel/src/architecture/mod.rs:5-15` carried the same stale "6. Idle loop"
TODO as F-001. `kstart()` in `main.rs` starts the scheduler and bootstraps the
init process. The comment was stale and misleading; overwritten by the new
documentation header.

### F-003 — Broken rustdoc example in mm/mod.rs (resolved)

`kernel/src/mm/mod.rs:42-46` had a doc example using `any PageAllocator
implementation` as placeholder text where a real type name belonged. The
example would not compile. Overwritten by the new documentation header in this
pass. Real examples should use `PmmPageAllocator` or `MockPageAllocator`.

### F-005 — Stale submodule list in architecture/x86_64/mod.rs comment (resolved)

`kernel/src/architecture/x86_64/mod.rs:1-21` listed a `peripheral` submodule
that does not exist. The actual submodules are `abi_check`, `apic`, `gdt`,
`idt`, `interrupts`, `pic`, `syscall`, `spectre`, `tsc`. Overwritten by the
new header.

## Plan lessons — kernel

Distilled implementation lessons from kernel-touching plans. 2-5 lines
each; see the dated plan file for the long form. The kernel is frozen
through ~2026-10-21; kernel commits land only when naming the userspace
failure that forced them.

### pat-msr-write-combining (2026-05-09-framebuffer-perf-wc)

Program the x86_64 PAT MSR (0x277) at boot to install a Linux-compatible
layout where index 1 = WC. UC-, UC, and WB stay where firmware put them;
existing PTE encodings keep their current semantics. `MAP_DEVICE_WC = 0x200`
is the new `SpaceMap` flag; PTE bits PCD=0, PWT=1, PAT=0 → index 1 → WC.
The new flag bit is unused on old kernels, so the same flag value falls
back gracefully. `map_device_page_wc` is mostly a copy of
`map_device_page` with WC flags. WC perf gain is real only under KVM or
baremetal — QEMU TCG treats every memory type as WB.
