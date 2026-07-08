---
slug: reliable-single-cpu-os
status: interviewing
intent: clear
pending-action: write .omo/plans/reliable-single-cpu-os.md
approach: Three-phase plan: (1) threading/memory quick-fixes to de-risk multi-threaded userspace, (2) driver framework skeleton with bus/driver traits + IRQ routing + DMA API + USB xHCI example, (3) dynamic linking with ld.so + PLT/GOT + shared lib format. Single-CPU only — SMP scaffolding preserved but not extended.
---

# Draft: reliable-single-cpu-os

## Components (topology ledger)
<!-- Lock the SHAPE before depth. One row per top-level component that can succeed or fail independently. -->
<!-- id | outcome (one line) | status: active|deferred | evidence path -->

| id | outcome | status | evidence |
|----|---------|--------|----------|
| C1 | Threading correctness fixes (errno, stack size, allocator blocking, detached leak) | active | errno.rs:101, pthread.rs:366/600, allocator.rs:776 |
| C2 | Memory correctness fixes (mprotect PROT_NONE, doc staleness) | active | posix/memory.rs:610, interpreter_porting.md:46, README.md:25 |
| C3 | MicroPython GC thread scan (mp_thread_gc_others stub) | active | mpthreadport.c:224 |
| C4 | Driver framework skeleton (bus/driver traits, devmgr registry, IRQ routing, DMA API) | active | devmgr sync leaf, virtio-blk bespoke, hardcoded IRQ vectors idt.rs:128 |
| C5 | USB xHCI driver skeleton (host controller + HID device driver) | active | no USB exists, QEMU supports xHCI |
| C6 | ACPI minimal (shutdown path + device enumeration) | active | no ACPI exists, main.rs shutdown is Ctrl-Alt-Del |
| C7 | Dynamic linking (ld.so, PLT/GOT, shared lib format, TLS variant) | active | all ELFs static, VFS map_elf_segments |

## Open assumptions (announced defaults)
<!-- Record any default you adopt instead of asking, so the user can veto it at the gate. -->
<!-- assumption | adopted default | rationale | reversible? -->

| assumption | adopted default | rationale | reversible? |
|------------|-----------------|-----------|-------------|
| Allocator contention fix | Blocking lock with re-entrant deferred-free guard (not per-thread nursery) | Nursery sweep fix already showed the pattern; blocking is correct; try_lock silent OOM is the bug | YES — can add per-thread nursery later |
| devmgr async model | Stays sync (leaf service); async callers use IpcCallFuture | Matches AGENTS.md §7 pattern; devmgr has no downstream IPC | YES |
| IRQ routing | Dynamic request_irq registration via devmgr broker | Current hardcoded vectors don't scale; devmgr is the natural broker | YES |
| USB scope | xHCI only (QEMU-native, modern spec) | QEMU uses xHCI by default; UHCI/OHCI are legacy | YES — can add legacy later |
| Shared lib format | Standard ELF ET_DYN | Interoperability with existing toolchain; no CLUU-specific format | YES |
| ld.so location | Userspace binary at /lib/ld-cluu.so | Matches Unix model; kernel exec path loads interpreter | YES |
| Symbol resolution | Eager (load-time, not lazy) | Simpler; no signal-based lazy trap; CLUU has no async signal delivery anyway | YES — can add lazy later |
| dlopen/dlsym | Deferred to follow-up phase (exec-time linking first) | Exec-time is the foundation; dlopen adds runtime complexity | YES |
| ACPI scope | Minimal: shutdown + device enumeration (no AML interpreter) | Full AML is a huge subsystem; minimal covers S-state shutdown + PCI device discovery | YES — can add AML later |
| Driver capability model | Driver gets capability token for device BAR + IRQ | Fits CLUU's existing cap model (AGENTS.md §2); no new syscalls | YES |
| Test strategy | TDD for framework traits; harness QA for integration; existing probes must still pass | Matches repo convention; harness is the gate | NO |

## Findings (cited - path:lines)

### From 4 explore agents (bg_9fd98e1a, bg_70824107, bg_3be98e61, bg_71749e2b)

**Threading bugs (real today, not SMP):**
- errno.rs:101-107 — `ERRNO_BY_THREAD` keyed by `token_self()` which is PROCESS token (boot.rs:300-302, same for all threads). Two pthreads collide on same Box<i32>.
- pthread.rs:366 — `pthread_create` hardcodes `DEFAULT_STACK_PAGES=16` regardless of `pthread_attr_setstacksize`.
- pthread.rs:600 — detached thread stack/TLS leak (known limitation, commented).
- allocator.rs:776 — `try_lock` failure returns null (silent OOM). Should block for multi-threaded contention.
- allocator.rs:670-696 — C2 deferred-free IS landed (gotchas.md says "planned" — stale).
- mpthreadport.c:224-234 — `mp_thread_gc_others` is a stub, traverses thread list but doesn't scan stacks.

**Memory fixes:**
- posix/memory.rs:610-614 — `mprotect(PROT_NONE)` returns ENOSYS. Kernel `space_protect` exists (InvokeOp), userspace wrapper refuses.
- doc/book/interpreter_porting.md:46,91-93 — says "no stack growth"; contradicted by memory_model.md:89-94 and stackgrow probe. Stack demand-pages to 16MB.
- README.md:25 — says MicroPython "no threading"; mpconfigport.h:33-34 has MICROPY_PY_THREAD=1 + GIL. Stale.

**Driver framework:**
- devmgr is sync leaf service (userspace/devmgr/src/main.rs). Registers devices, routes open/read/write/ioctl.
- virtio-blk is hand-written (userspace/virtio-blk/src/): PCI scan, virtqueue, DMA pool, IRQ handler — all bespoke.
- IRQ vectors hardcoded (idt.rs:128-132): IRQ1=kbd, IRQ4=serial, IRQ11=virtio-blk, IRQ12=mouse. No dynamic registration.
- DmaPool exists in virtio-core/src/dma.rs — generalized DMA pool with alloc/phys_of. Good starting point.
- No USB, no networking, no sound, no ACPI.
- APIC minimal (apic.rs:4): timer only, no IOAPIC. IRQs via legacy 8259 PIC (remapped+masked).

**Dynamic linking:**
- All ELFs static. VFS map_elf_segments (vfs/src/main.rs:5184) maps segments via space_map_range.
- Kernel ELF loader (kernel/src/elf.rs load_segment_batch) handles PT_LOAD.
- No PT_INTERP, no DT_DYNAMIC, no .so files, no dlopen.
- crt0.S (userspace/newlib/crt0.S) is the static entry point.

## Decisions (with rationale)

1. **Threading fixes first** — de-risks multi-threaded programs before architectural work. Quick wins.
2. **Driver framework second** — shapes how all new device code is written. USB xHCI as the concrete example.
3. **Dynamic linking last** — most self-contained, doesn't block driver work.
4. **Single-CPU only** — SMP scaffolding preserved, not extended. No AP startup, no IPI, no TLB shootdown.

## Scope IN

- C1: errno keying fix, pthread stack size honored, allocator blocking lock, detached thread cleanup
- C2: mprotect(PROT_NONE) wired to kernel space_protect, doc updates (interpreter_porting.md, README.md, gotchas.md)
- C3: mp_thread_gc_others stack scanning implementation
- C4: driver framework crate with BusDriver/DeviceDriver/IrqHandler traits, devmgr registry, IRQ routing table, generalized DMA API
- C5: xHCI host controller driver skeleton + USB HID device driver skeleton (mouse/keyboard)
- C6: ACPI minimal — shutdown path (S5), PCI device enumeration
- C7: ld.so userspace binary, PLT/GOT eager resolution, ET_DYN shared lib format, TLS variant for dynamic TLS

## Scope OUT (Must NOT have)

- SMP (no AP startup, no IPI, no TLB shootdown, no per-CPU run queues)
- dlopen/dlsym runtime loading (deferred to follow-up)
- Lazy symbol resolution (eager only)
- UHCI/OHCI USB controllers (xHCI only)
- Full ACPI AML interpreter (minimal shutdown + enumeration only)
- Kernel-side drivers (all drivers are userspace services)
- New syscalls (use InvokeOp variants on existing token-dispatch path per AGENTS.md §2)
- Runtime ACL (capability tokens + VFS view scoping only per AGENTS.md §3)
- Network stack (TCP/IP — separate future work; this plan delivers the driver framework that a net driver would use)
- Sound subsystem (audio driver framework comes after USB; this plan delivers the driver framework that a sound driver would use)

## Open questions

### Q1: Driver model — userspace services vs kernel modules?
**Why**: CLUU is seL4-inspired (AGENTS.md §1) — drivers should be userspace services with capability tokens for device BAR+IRQ. But some drivers (interrupt controller, early DMA) might need kernel-side for boot. This forks the entire driver framework architecture.
**Explored**: devmgr is userspace (userspace/devmgr/). virtio-blk is userspace. All device access goes through IPC. Kernel only knows threads, caps, IPC.
**Default**: All drivers userspace. Kernel exposes a `map_device_region` InvokeOp (maps MMIO BAR into driver's space) + `request_irq` InvokeOp (routes IRQ to driver's endpoint). ACPI shutdown might need a kernel-side hook.
**Fork**: Does the user want any kernel-side drivers, or strictly userspace-only?

### Q2: Dynamic linking scope — exec-time only vs dlopen?
**Why**: Exec-time linking (ld.so runs at process start, resolves all symbols, jumps to entry) is the foundation. dlopen (runtime loading of .so files) adds significant complexity: reference counting, symbol namespace isolation, destructor ordering, TLS generation counter. This forks the dynamic linking workstream size by ~3x.
**Explored**: No existing dynamic linking. crt0.S is static entry. VFS map_elf_segments handles PT_LOAD only.
**Default**: Exec-time only for this plan. dlopen deferred to follow-up.
**Fork**: Does the user want dlopen in this plan, or is exec-time sufficient for now?

### Q3: ACPI scope — minimal vs full AML?
**Why**: Minimal ACPI (S5 shutdown + PCI device enumeration via static tables) is ~2-3 days. Full AML interpreter (dynamic device discovery, thermal zones, power states, _PRS/_CRS resource extraction) is a multi-week subsystem. This forks C6 size dramatically.
**Explored**: No ACPI exists. Shutdown is Ctrl-Alt-Del (main.rs:315). PCI enumeration is hand-coded in virtio-blk.
**Default**: Minimal — S5 shutdown + PCI enumeration from static MCFG/RSDP tables. No AML byte-code interpreter.
**Fork**: Does the user want full ACPI AML, or is minimal sufficient for a reliable single-CPU OS?

## Approval gate
status: awaiting-approval
pending-action: write .omo/plans/reliable-single-cpu-os.md
approach: Three-phase plan, 7 components:
- C1-C3 (Phase 1): Threading/memory quick-fixes (errno keying, stack size, allocator blocking, detached leak, mprotect PROT_NONE, doc staleness, mp_thread_gc_others). De-risks multi-threaded userspace.
- C4-C6 (Phase 2): Driver framework skeleton (bus/driver traits, devmgr registry, IRQ routing, DMA API) + USB xHCI driver skeleton + minimal ACPI (S5 shutdown + PCI enumeration). All drivers userspace. Kernel gets map_device_region + request_irq InvokeOps.
- C7 (Phase 3): Dynamic linking (ld.so, PLT/GOT eager resolution, ET_DYN shared lib format, TLS variant). Exec-time only, no dlopen.

Resolved forks:
- Q1: Userspace-only drivers (recommended)
- Q2: Exec-time only dynamic linking (recommended)
- Q3: Minimal ACPI (recommended)
