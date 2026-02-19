# CLUU Status and Plan — February 2026

**Consolidation of**: `DEEP_DIVE_ANALYSIS.md`, `docs/archive/kernel-maturity-analysis-feb-2026.md`, five research audits (POSIX layer, kernel capabilities, MicroPython/Quake, DMA/drivers, namespacing/security)
**Constraints**: No new syscalls (use Invoke), seL4-inspired kernel with POSIX compat layer, single-core, minimal drivers
**Goals**: Port and run MicroPython, Quake 1 (software renderer, no sound); build toward general-purpose console OS with device drivers and networking
**Automated verification**: All changes can be validated end-to-end without manual testing. The harness stack (`test_hello.sh` + 36+ marker modes + SLO enforcement) boots CLUU in headless QEMU, injects commands, captures serial output, and asserts correctness markers and performance thresholds. Run `cargo xtask harness-matrix` to gate any commit against the full regression suite.

---

## 1. Where We Are

### Kernel (~20K LOC) — Production-quality for single-CPU

| Subsystem | Grade | Notes |
|---|---|---|
| Syscall ABI | A | 7 syscalls + 37 invoke ops. Stable ABI |
| Scheduler | A | O(1) priority bitmap, active/expired fairness, 256 priority levels |
| IPC | A | Queue + rendezvous + register fast path + shared-ring bulk. Sender auth. recv_any (16 endpoints) |
| PMM | A | Buddy allocator (orders 0-9, 4KB-2MB), two-phase init, O(1) alloc, coalescing |
| VMM | A- | Demand paging, SpaceProtect, SpaceGrant, SpaceMapRange. teardown_user_pages on exit. No COW |
| Token/Cap | B+ | HMAC-SHA256 signed, subset derivation, revocation. Global handles (accepted for single-user) |
| Fault IPC | A | seL4-style fault endpoints, full register context, reply-based resume/kill |
| Futex | A | Space-scoped wait queues, timeout, value-compare gate. Race-validated |
| Threading | A- | FS base save/restore on all 9 return-to-userspace paths, ThreadSetFSBase invoke |
| Frame caps | A- | FrameAllocate/Free/GetPhys, map-count tracking, owner space, phys lookup |
| Telemetry | A | Atomic counters, histograms, audit ring, 36+ harness modes |

**Kernel gaps** (from audit):
- `IrqAck` (op 31): stubbed as `NotImplemented` — blocks device drivers
- `ThreadSetPriority` (op 4): not implemented
- No MSI/MSI-X support (PIC only, 16 IRQs max)
- No IOMMU (acceptable for QEMU, needed for real hardware)
- 4GB physical memory limit (MAX_FRAMES = 1M × 4KB)
- IRQ dispatch is keyboard-specific (`dispatch_scancode`), not generic

### Userspace Services (~25K LOC) — Functional

| Service | Status | Notes |
|---|---|---|
| procmgr | Working | Spawn with envp/fd inheritance, kill, waitpid, exit notifications |
| VFS | Working | Ext2 read+write, mkdir/rmdir/rename/unlink, per-path owner, DeviceBackend |
| console | Working | ANSI CSI, SGR colors, UTF-8→CP437, raw mode |
| tty | Working | Line discipline, Ctrl-C, termios, poll readiness query |
| kbd | Working | PS/2 keyboard, scan code translation |
| shell | Working | 32 builtins, bg/fg/jobs, spawn, command history |
| registry | Working | Service name lookup, sender-auth bound |
| timeserver | Working | Clock service (CLOCK_REALTIME, CLOCK_MONOTONIC) |
| virtio-blk | Working | Block device driver (4MiB grant windows) |
| ext2 | Working | Read+write, inode/block alloc, dir ops |

### POSIX Layer (libcluu) — 105 functions working

**Complete**: file I/O (open/close/read/write/lseek/dup/dup2/mkdir/rmdir/rename/unlink), stat/fstat/isatty, processes (exit/getpid/kill/waitpid/posix_spawn/system), directories (opendir/readdir/closedir/getcwd/chdir), memory (sbrk/mmap/munmap/mprotect/msync), time (gettimeofday/clock_gettime/sleep/usleep/nanosleep), pthreads (create/join/detach/self/mutex/cond/once/key — 1046 lines), pipes (pipe/read/write/EOF/SIGPIPE), env (getenv/setenv/unsetenv/environ), fcntl (F_DUPFD/F_GETFD/F_SETFD/F_GETFL/F_SETFL), poll/select, termios (tcgetattr/tcsetattr/ioctl TIOCGWINSZ), framebuffer (acquire/release), signals (signal/sigaction/raise — userspace only), device files (/dev/null /dev/zero /dev/urandom), file-backed mmap (MAP_PRIVATE copy-on-map), stubs (access/chmod/chown/getuid/getpwnam/mkstemp/realpath/basename/dirname).

**Stubbed**: fork (ENOSYS), execve (ENOSYS), _link (ENOSYS).

**Not implemented**: sockets (any), semaphores, shared memory (SysV), async I/O, readv/writev, pread/pwrite, pthread_cancel, pthread_barrier, pthread_rwlock, sigprocmask, kernel-level signal delivery, F_SETLK/advisory locking.

---

## 2. Completed Work Packages

| Phase | Work | Status |
|---|---|---|
| M0-M5 | Telemetry, manifest, waitset recv, token audit, leak diag, failpoints, sender auth, fairness SLOs | DONE |
| M6/P0.1-P0.5 | Compact message queue, rendezvous fast path, shared-ring bulk, register IPC fast path | DONE |
| B.3 | Spawn hot-path (ELF metadata cache, VFS handle cache) | DONE |
| C.1 | Futex wait/wake via Invoke (space-scoped, timeout, race harness) | DONE |
| L2A | Ext2 write, mkdir/rmdir/rename/unlink, O_CREAT, owner-auth | DONE |
| L2B | Ctrl-C, bg/fg/jobs, stop/resume, waitpid WNOHANG | DONE |
| L2C | mmap first-fit + munmap + mprotect via SpaceProtect | DONE |
| Phase 5 | Fault handling → IPC (seL4-style), GPF/PF assembly, schedule_next_from_fault | DONE |
| Phase 6 | Frame capabilities (FrameAllocate/Free/GetPhys, MAP_FRAME_TOKEN) | DONE |
| Phase 7 | Buddy allocator PMM (orders 0-9, intrusive free lists, coalescing) | DONE |
| Phase 1 | Foundation: setjmp verification, env vars, POSIX stubs | DONE |
| Phase 2 | Pipe + Process: pipe(), fd inheritance, SpaceDestroy, teardown_user_pages | DONE |
| Phase 3 | Threading: TLS (FS base, .tdata/.tbss), pthreads (1046 LOC), signals (SIGPIPE/SIGCHLD) | DONE |
| Phase 4 | I/O: TTY poll, framebuffer MMIO, device files, file-backed mmap | DONE |

---

## 3. Gap Analysis: What Blocks the Next Goals

### Application Porting Gaps

| Gap | Blocks | Effort | Notes |
|---|---|---|---|
| **Raw keyboard scancodes** | Quake (key up/down events) | MEDIUM | TTY delivers cooked characters. Need raw scancode service or kbd IPC mode |
| **Mouse input** | Quake (aiming) | MEDIUM | PS/2 mouse driver (IRQ12 + I/O ports 0x60/0x64). 3-byte packet protocol |
| **8-bit→32-bit palette blit** | Quake (rendering) | LOW | ~50 LOC tight loop in vid_cluu.c. Framebuffer already MMIO-mapped |
| **sched_yield** | MicroPython (thread idle) | LOW | Stub or wire to sys_yield |
| **pthread_cancel** | MicroPython (unix port) | LOW | Can patch MicroPython to use cooperative exit flag instead |
| **fork/exec shim** | mksh | HIGH | mksh uses fork internally; needs source patching or vfork→posix_spawn translation |

### Device Driver Gaps (kernel primitives)

| Gap | Blocks | Effort | Notes |
|---|---|---|---|
| **IrqAck not implemented** | All device drivers | LOW | Send EOI + re-enable IRQ line. Op 31 already allocated |
| **IRQ dispatch is keyboard-only** | Non-keyboard devices | LOW | Generalize to send IRQ number notification to attached endpoint |
| **No arbitrary-order alloc** | DMA structures (virtqueues, BDLs) | LOW | Expose pmm::alloc_order(1-8) via new invoke op |
| **No MSI-X support** | Modern device drivers | HIGH | Vector allocation, IDT entry setup, PCI capability parsing |

### Networking Gaps

| Gap | Blocks | Effort | Notes |
|---|---|---|---|
| **No virtio-net driver** | All networking | HIGH | PCI enumeration + MMIO BAR + virtqueue DMA + IRQ |
| **No TCP/IP stack** | Socket API | HIGH | lwIP port or custom minimal stack |
| **No socket POSIX API** | curl, Python networking | MEDIUM | socket/bind/listen/accept/connect/send/recv over IPC to network service |

### Security Gaps (not blocking current goals)

| Gap | Impact | Effort | Notes |
|---|---|---|---|
| **No UID tracking** | Multi-user | LOW | procmgr-level policy, zero kernel changes |
| **No capability manifests** | App sandboxing | MEDIUM | ELF section or sidecar file declaring needed tokens |
| **No registry namespacing** | Service isolation | MEDIUM | procmgr controls which services each process can discover |
| **No IOMMU** | DMA security | HIGH | Intel VT-d, requires QEMU q35 machine type |

---

## 4. Porting Feasibility Per Target (Updated)

### MicroPython — READY TO PORT

All required POSIX functions exist. The MicroPython minimal/unix port needs:

| Requirement | CLUU Status |
|---|---|
| read/write (stdin/stdout) | HAVE |
| malloc/free (newlib) | HAVE |
| gettimeofday, clock_gettime | HAVE |
| tcgetattr/tcsetattr (raw mode REPL) | HAVE |
| pthread_create/join, mutex lock/unlock/trylock | HAVE |
| TLS (__thread, FS base) | HAVE |
| getenv | HAVE |
| open/close/read/write/lseek/stat/fstat | HAVE |
| opendir/readdir/closedir | HAVE |
| signal/sigaction | HAVE |
| Static linking (newlib) | HAVE |
| sched_yield | Need stub (trivial) |
| pthread_cancel/setcanceltype | Patch MicroPython (use cooperative exit) |

**Porting approach**: Create `ports/cluu/` based on `ports/minimal`, borrowing config from `ports/unix`. Key files: `mpconfigport.h` (enable VFS_POSIX, PY_THREAD; disable PY_FFI, PLAT_DEV_MEM), `mphalport.c` (character I/O), `mpthreadport.c` (wrap pthreads, omit pthread_cancel), `Makefile` (cross-compile with x86_64-cluu-elf-gcc).

**Effort**: Small. MicroPython already runs on bare-metal ARM with newlib.

### Quake 1 (WinQuake Software Renderer) — NEARLY READY

Single-threaded game with clean OS abstraction layer (`sys_*.c`, `vid_*.c`). All file I/O, timing, and memory ready.

| Requirement | CLUU Status |
|---|---|
| malloc (16-32 MB hunk) | HAVE (heap up to ~1 GB) |
| open/close/read/write/lseek/stat | HAVE |
| gettimeofday (microsecond timer) | HAVE |
| usleep (frame rate limiting) | HAVE |
| getenv (config paths) | HAVE |
| _exit (clean shutdown) | HAVE |
| printf/snprintf (console) | HAVE (newlib) |
| Framebuffer (MMIO map, 32-bit BGRA) | HAVE |
| **Raw keyboard scancodes (up/down)** | **NEED** — biggest gap |
| **Mouse input (relative deltas + buttons)** | **NEED** — essential for gameplay |
| Sound output | SKIP (-nosound flag) |
| Networking | SKIP (-nolan flag) |
| mprotect (x86 asm optimizations) | HAVE (but compile C-only mode initially) |

**Porting approach**: Write `sys_cluu.c` (trivial — map Sys_* to POSIX), `vid_cluu.c` (8-bit palette→32-bit BGRA blit to MMIO framebuffer), `in_cluu.c` (raw keyboard/mouse IPC). Use `snd_null.c`, `cd_null.c`, `net_none.c`. Compile C-only mode (no x86 asm) with x86_64-cluu-elf-gcc.

**Blocking work**: Raw keyboard scancode delivery and PS/2 mouse driver.

**Effort**: Medium. OS plumbing ready; input subsystem is the real work.

### mksh — DEFERRED

Requires fork() internally for job control and pipelines. Would need significant source patching (vfork+exec → posix_spawn shim). Lower priority than MicroPython and Quake. CLUU's built-in shell handles basic use cases.

### CCC (Claude's C Compiler) — FEASIBLE

Can be designed for CLUU from scratch: use posix_spawn, write temp files instead of pipes. Needs: setjmp (HAVE), mkstemp (HAVE), environment (HAVE).

---

## 5. Execution Plan

### Phase 5: Application Ports (MicroPython + Quake foundation)

**5.1 MicroPython port**
- Create `ports/cluu/` directory with mpconfigport.h, mphalport.c, mpthreadport.c, main.c, Makefile
- Configure: MICROPY_PY_THREAD=1, MICROPY_PY_FFI=0, MICROPY_VFS_POSIX=1
- Patch out pthread_cancel usage (use cooperative thread exit flag)
- Add sched_yield stub (wire to sys_yield or no-op)
- Build: cross-compile with x86_64-cluu-elf toolchain, link against newlib + libcluu
- Pack into initrd, test via harness: boot → spawn micropython → `print("hello")` → validate marker
- Validation: `p5_micropython` harness mode — REPL boots, `print("hello")` output captured

**5.2 Raw keyboard input service**
- Extend kbd service to support two modes: cooked (current, characters via TTY) and raw (scancodes with up/down events via direct IPC)
- New IPC label for kbd: `KBD_RAW_SUBSCRIBE` — client gets raw scancode events (make/break codes)
- Scancode events: `{scancode: u8, pressed: bool}` per event
- Client library in libcluu: `kbd_raw_open()`, `kbd_raw_read()` returning KeyEvent structs
- This unblocks Quake and any future game/editor that needs raw key input
- Validation: `p5_rawinput` harness mode — probe subscribes to raw kbd, receives expected scancodes

**5.3 PS/2 mouse driver**
- New userspace service: `mousedrv`
- Kernel side: attach IRQ12 to endpoint via IrqAttach (requires IrqAck implementation first)
- Enable PS/2 auxiliary port (command 0xA8 to port 0x64), enable IRQ12, send 0xF4 (enable data reporting)
- 3-byte packet accumulation: buttons + X/Y deltas per 3 interrupts
- Service provides `MOUSE_EVENT` IPC messages: `{dx: i16, dy: i16, buttons: u8}`
- Client library: `mouse_open()`, `mouse_read()` returning MouseEvent
- Validation: `p5_mouse` harness mode (may need QEMU `-device virtio-mouse-pci` or PS/2 emulation)

**5.4 Quake 1 port**
- Write `sys_cluu.c`: map Sys_FloatTime→gettimeofday, Sys_FileOpen→open, etc.
- Write `vid_cluu.c`: framebuffer_acquire(), 256-entry palette LUT (8-bit→32-bit BGRA), VID_Update blit loop
- Write `in_cluu.c`: kbd_raw_read() for keyboard, mouse_read() for mouse, map to Quake key codes
- Use `snd_null.c`, `cd_null.c`, `net_none.c`
- Compile with `-DQUAKE_NO_ASM` (C-only renderer, ~50% slower but avoids mprotect complexity)
- Pack Quake binary + id1/pak0.pak into boot image
- Validation: `p5_quake` harness mode — Quake renders first frame (detect framebuffer write pattern)

### Phase 6: Kernel Driver Primitives (unblocks device ecosystem)

**6.1 IrqAck implementation**
- Implement invoke op 31 (IrqAck): send EOI to PIC for given IRQ, re-enable IRQ line
- Wire into invoke dispatch in handlers.rs
- Required for: PS/2 mouse, AC97, virtio-net, any device that generates multiple interrupts

**6.2 Generic IRQ dispatch**
- Refactor `dispatch_scancode()` in irq.rs to generic `dispatch_irq(irq_num)`
- Send IPC message with label=IRQ_NUMBER to attached endpoint
- Driver reads device status register to determine what happened
- Keyboard driver adapts to use generic path (backward compatible)

**6.3 Arbitrary-order contiguous allocation**
- New invoke op: `PmmAllocOrder` — allocate 2^order contiguous pages (orders 0-8)
- Returns physical address of allocated block
- Companion: `PmmFreeOrder` — free with correct order
- Needed for: virtqueue descriptor tables, AHCI command lists, DMA buffers
- Wire through frame_registry for proper cleanup tracking

### Phase 7: Networking (virtio-net + TCP/IP)

**7.1 virtio-net driver**
- Userspace service using PCI enumeration (vendor 0x1AF4, device 0x1041)
- Map device MMIO BAR via SpaceMap + MAP_DEVICE
- Set up split virtqueues (rx + tx): descriptor table + available ring + used ring
- Allocate DMA buffers via PmmAllocOrder, get physical addresses
- IRQ handling via IrqAttach + IrqAck
- Packet send/receive via virtqueue kick + IRQ notification

**7.2 TCP/IP stack**
- Port lwIP (lightweight TCP/IP) or write minimal custom stack
- Ethernet frame → ARP + IP → TCP/UDP
- Socket-like API exposed via IPC service
- Service accepts connections, manages TCP state, delivers data to clients

**7.3 Socket POSIX API**
- socket/bind/listen/accept/connect/send/recv/close
- Implemented in libcluu as IPC wrappers to TCP/IP service
- sendto/recvfrom for UDP
- gethostbyname stub (static /etc/hosts or hardcoded DNS)

### Phase 8: Security Hardening (incremental, no kernel redesign)

**8.1 Procmgr UID tracking**
- Add `uid` field to procmgr's per-process state
- Spawn sets uid based on parent's uid
- Token set granted to child determined by uid policy
- Zero kernel changes — purely userspace policy

**8.2 Registry namespacing**
- Procmgr controls which registry entries each process group can discover
- Sandboxed process cannot find services it shouldn't access
- Prevents: untrusted code connecting to display, network, device services

**8.3 Capability manifests (optional)**
- Embed capability requirements in ELF `.cluu.caps` section or sidecar file
- Procmgr reads manifest at spawn time, grants only declared tokens
- Least-privilege enforcement without manual configuration

---

## 6. What We Deliberately Skip

| Item | Reason |
|---|---|
| fork() | Incompatible with microkernel. Use posix_spawn. Patch ports as needed |
| SMP | Single-core is the accepted scope. Avoids TLB shootdown, lock ordering |
| Per-process CSpace | Global handle table is fine for trusted single-user |
| Dynamic linking | Static linking works. No dlopen/dlsym |
| COW / demand-load | MAP_PRIVATE copy-on-map is sufficient |
| Full POSIX signals | Userspace signal handlers + SIGPIPE/SIGCHLD. No kernel-driven async delivery |
| /proc, /sys | Not needed for stated goals |
| IOMMU | Acceptable for QEMU. Revisit for real hardware |
| Sound (Phase 5) | Quake runs with -nosound. AC97 driver is Phase 9+ |
| USB (XHCI) | Most complex device. Defer until after networking |

---

## 7. Validation Strategy

### Harness Infrastructure

| Component | Role |
|---|---|
| `test_hello.sh` | Single-run QEMU executor. Boots, injects commands, validates markers |
| `scripts/harness_cases.conf` | Central case catalog |
| `scripts/harness_suite.sh` | Generic case runner (--no-build, --case, --list) |
| `scripts/harness_matrix.sh` | Full regression gate (`cargo xtask harness-matrix`) |
| `scripts/harness_slo_report.sh` | SLO parser for serial logs |

**Existing harness modes** (36+ cases): m1_recv, m2_token_audit, m2_leakdiag, m3_mapfail, m3_mapcopyfail, m3_maperror, m4_sender_auth, m4_registry_sender_auth, m4_notify_lifecycle, m4_deny_paths, m4_registry_deny_paths, m5_fairness, m6_ipc_compact, m6_ipc_rendezvous, m6_ring_io, m6_ipc_reg_off, m6_ipc_reg_on, l2_ext2write, l2_ext2append, l2_ext2mutate, l2_ext2unlink, l2_owner_deny, l2_sigint, l2_jobs, l2_fg, l2_stop, l2_jobchurn, l2_jobchurn_heavy, l2_jobmix, l2_waitpid, l2_mmap, a_poll, perf_benchprobe, b_spawn_perf, b_spawn_warm, c_futex, c_futex_race + phase 1-4 probes (p1_setjmp, p1_env, p1_stubs, p2_pipe, p2_spawn_fd, p3_pthread, p3_tls, etc.).

### New Phase Gates

| Phase | Harness mode | Gate criteria |
|---|---|---|
| 5.1 | `p5_micropython` | MicroPython REPL boots, `print("hello")` captured |
| 5.2 | `p5_rawinput` | Raw scancode events received via IPC |
| 5.3 | `p5_mouse` | Mouse events received (may need QEMU emulation) |
| 5.4 | `p5_quake` | Quake binary starts, renders first frame |
| 6.1 | `p6_irqack` | IRQ ack round-trip for keyboard (existing IRQ1 path) |
| 6.2 | `p6_generic_irq` | Non-keyboard IRQ delivered to endpoint |
| 6.3 | `p6_alloc_order` | Multi-page contiguous alloc + phys addr verification |
| 7.1 | `p7_virtio_net` | Ping reply from virtio-net driver |
| 7.3 | `p7_socket` | TCP connect + send + recv round-trip |

**Regression policy**: All existing harness modes must pass after each phase.

---

## 8. Device Driver Bring-Up Order

Based on research into x86_64 DMA, device complexity, and CLUU's existing kernel primitives:

| Order | Device | Complexity | Kernel Needs | Purpose |
|---|---|---|---|---|
| 1 | **PS/2 Mouse** | Low | IrqAck, generic IRQ dispatch | Quake input, GUI foundation |
| 2 | **AC97 Sound** | Medium | + contiguous alloc (BDL) | Audio output for Quake |
| 3 | **virtio-net** | Medium-High | + MMIO BAR, DMA virtqueues | Networking |
| 4 | **AHCI/SATA** | Medium-High | Same as virtio-net | Real hardware storage |
| 5 | **virtio-gpu** | Medium | Same as virtio-net | Hardware-assisted rendering |
| 6 | **XHCI USB** | Very High | + MSI-X | USB devices (keyboard, mouse, storage) |

**Key facts** (from DMA research):
- x86_64 has hardware cache coherency for DMA — no software cache maintenance needed
- No IOMMU needed for QEMU (devices only DMA to programmed addresses)
- No bounce buffers needed (QEMU RAM < 4GB, all devices support 32-bit+ DMA)
- CLUU's buddy allocator already supports contiguous alloc up to 2MB — sufficient for all device structures

---

## 9. Security Architecture (Capability-Based)

CLUU's token system already embodies the capability model used by seL4, Genode, and Fuchsia. Key insight from research: **every CLUU process is already in a "container"** — it can only access resources for which it holds tokens. The remaining work is userspace policy, not kernel changes.

### Incremental security path

1. **Now**: Document current token distribution policy (which tokens does each process type get?)
2. **Phase 8.1**: Add `uid` to procmgr process tracking. Zero kernel changes
3. **Phase 8.2**: Registry namespacing — procmgr controls service visibility per process group
4. **Phase 8.3**: Capability manifests in ELF or sidecar files
5. **Later**: Login service authenticates users, sets uid for sessions → determines token profiles

### Token profiles (for Phase 8.1)

| Profile | Tokens granted | Use case |
|---|---|---|
| **privileged** | Full rights (IPC, memory, PCI, port, IRQ, frame) | Drivers, system services |
| **standard** | IPC + memory + VFS + TTY | Normal applications |
| **restricted** | Minimal IPC, read-only VFS, no device access | Untrusted code |

---

## 10. Priority Summary

```
Phase 5: Application Ports    MicroPython, raw input, mouse, Quake    → flagship demos running
Phase 6: Driver Primitives    IrqAck, generic IRQ, alloc_order        → device driver ecosystem
Phase 7: Networking            virtio-net, TCP/IP, socket API          → connected OS
Phase 8: Security              UID tracking, registry namespace        → multi-user foundation
```

Phase 5 is the immediate focus. MicroPython can be ported with minimal effort. Quake needs the raw input + mouse work first. Phases 6-8 build out the platform for long-term viability.

---

## 11. Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| MicroPython pthread_cancel usage | MEDIUM | Port won't build | Patch MicroPython to use cooperative exit (trivial) |
| Quake expects X11/SDL input model | HIGH | Need custom input layer | Write in_cluu.c (~200 LOC) over raw kbd/mouse IPC |
| PS/2 mouse 3-byte packet sync | MEDIUM | Garbled events | Byte alignment detection (check bit 3 of first byte) |
| Quake C-only mode too slow | LOW | Poor FPS | BOOTBOOT framebuffer is typically 1024x768; Quake at 320x200 with upscale is fine |
| virtio-net requires MSI-X | MEDIUM | Fallback to legacy IRQ | virtio supports legacy IRQ mode; MSI-X optional |
| lwIP port complexity | MEDIUM | Networking delayed | Start with raw Ethernet + ICMP (ping), add TCP incrementally |
| Token handle guessing | LOW (single-user) | Capability escape | Accepted risk. Fix later with per-process CSpace if needed |

---

## 12. POSIX Compatibility Summary (105 working / ~300+ total)

### What works (sufficient for MicroPython and Quake)

File I/O, stat, process lifecycle (posix_spawn/waitpid/exit/kill), directories, memory (sbrk/mmap/munmap/mprotect), time, pthreads (create/join/mutex/cond/once/key — full), pipes, environment, fcntl, poll/select, termios, framebuffer, signals (userspace), device files.

### What's missing (not needed for current goals)

Sockets (Phase 7), fork/exec (deliberate skip), kernel signals (deliberate skip), SysV IPC (unnecessary), async I/O (unnecessary), advisory locking (unnecessary), POSIX semaphores (use futex-based pthreads instead).

### What could break edge cases

- poll() uses 1ms polling loop (not kernel-driven notification)
- O_NONBLOCK flag tracked but not enforced by kernel
- nanosleep rounds to milliseconds (kernel timer granularity)
- No signal masks (sigprocmask) — signals cannot be blocked/deferred
- No pthread cleanup handlers (pthread_cleanup_push/pop)
