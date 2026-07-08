# reliable-single-cpu-os - Work Plan

## TL;DR (For humans)

**What you'll get:** A more reliable single-CPU OS: threading bugs fixed (per-thread errno, configurable stack size, no silent OOM, no detached-thread leaks), true `PROT_NONE` memory protection, MicroPython cross-thread garbage collection, a reusable driver framework with dynamic IRQ routing, USB xHCI + HID support, FADT-derived ACPI shutdown, and dynamic linking with a userspace ld.so.

**Why this approach:** Two independent deep analyses (Metis gap analysis + Opus repo-grounded study) converged on the same finding: most kernel primitives the original draft called for *already exist* (`IrqAttach`, `SpaceMap+MAP_DEVICE`, `space_protect`). The real work is consolidation and finishing, not building from scratch — except C7 (dynamic linking), which is genuinely greenfield. This plan adds ZERO new syscalls and ZERO new InvokeOps, holding the CLUU discipline.

**What it will NOT do:**
- No new syscalls or InvokeOp variants — reuses `IrqAttach=30`, `SpaceMap+MAP_DEVICE=0x100`, `SpaceProtect=16`
- No runtime ACL layer — capability tokens + VFS view scoping remain the sole authority model
- No AML interpreter for ACPI — QEMU uses static INTx; `_PRT` is out of scope
- No USB hubs, multi-device, isochronous transfers, or full HID descriptor parsing — boot-protocol only
- No lazy PLT resolution — eager (BIND_NOW) only

**Effort:** XL
**Risk:** Medium — C7 is greenfield and largest; C1-C4 are well-understood fixes with clear patterns
**Decisions to sanity-check:** (1) C2 uses `user:false` instead of `present:false` for PROT_NONE (keeps page restorable); (2) C7 Design B (VFS reports interp path, procmgr maps ld.so, auxv in ProcessInfo); (3) C3 uses esp32-port conservative whole-stack scan (no way to read peer registers from userspace)

Your next move: approve, or run a high-accuracy review. Full execution detail follows below.

---

> TL;DR (machine): XL effort, Medium risk. 17 todos across 3 phases. C1-C3 correctness, C4 driver framework, C5-C7 hardware+linking. Zero new syscalls/InvokeOps.

## Scope
### Must have
- **C1**: Per-thread errno, configurable pthread stack size, blocking allocator lock, detached thread stack reclamation
- **C2**: True `PROT_NONE` via kernel `space_protect` fix + userspace wrapper unblock, 3 doc staleness fixes
- **C3**: MicroPython `mp_thread_gc_others` cross-thread stack scanning
- **C4**: `driver-framework` crate (BusDriver/DeviceDriver/IrqHandler traits), dynamic IRQ trampoline replacing 5 bespoke IDT handlers, DmaPool extract to shared crate
- **C5**: `xhci-core` crate (PCI enum, controller reset, TRB rings), `usb-hid` crate (boot-protocol keyboard+mouse), QEMU USB config
- **C6**: `acpi` crate (RSDP discovery, FADT/MCFG parsing), FADT-derived S5 shutdown replacing hardcoded magic, MCFG ECAM exposure
- **C7**: `boot_elf.rs` ET_DYN acceptance, auxv infrastructure, `ld-cluu` crate (self-reloc, DT_NEEDED, reloc engine), dynamic TLS (`__tls_get_addr` + DTV)

### Must NOT have (guardrails, anti-slop, scope boundaries)
- **ZERO new syscalls** — AGENTS.md §2. All new functionality rides existing InvokeOps.
- **ZERO new InvokeOp variants** — `IrqAttach=30` (token/mod.rs:400) handles IRQ registration; `SpaceMap+MAP_DEVICE=0x100` (syscall.rs:136) handles MMIO mapping. Do NOT add `RequestIrq` or `MapDeviceRegion`. (Metis GAP-1/2/22)
- **No runtime ACL** — AGENTS.md §3. Authority is capability tokens + VFS view scoping, decided at spawn.
- **No AML interpreter** — QEMU uses static INTx lines from PCI config offset 0x3C. `_PRT` is out of scope. (Metis GAP-11)
- **No USB hubs/multi-device/isoc** — boot-protocol HID only, single device, additive to PS/2. (Metis GAP-10)
- **No lazy PLT resolution** — eager BIND_NOW. No signal-based lazy trap (CLUU has no async signal delivery).
- **No `as any`/`unwrap`/`@ts-ignore`** — AGENTS.md §9. `Result<T>` over panics, `debug_print` for serial diagnostics.
- **No dealloc blocking lock change** — only `alloc` gets blocking `lock()`. `dealloc` keeps `try_lock` + deferred-free to prevent re-entrant deadlock. (Metis GAP-7/25)
- **No `pid()` for errno keying** — use FS:8 per-thread token, matching `pthread_self()`. (Metis GAP-6/23)
- **No cross-session visibility leaks** — AGENTS.md §5/§6. Root godmode stays root-bound.

## Verification strategy
> Zero human intervention - all verification is agent-executed.
- Test decision: tests-after ( probes are the test framework; new probes added per component)
- Evidence: `.omo/evidence/task-<N>-reliable-single-cpu-os.<ext>`
- Baseline: run `python3 -m cluu_harness --list` and `python3 -m cluu_harness --no-build` before any work. Record the passing case list as the regression gate. After each phase, ALL baseline cases must still pass. New cases are ADDITIVE. (Metis GAP-21)
- Harness: `scripts/harness_run.sh` + `python3 -m cluu_harness` (doc/book/testing.md). Login creds: `root`/`root`.
- Build: `cargo xtask build`. Boot: `cargo xtask run` or harness.
- GDB for hangs: `QEMU_GDB=1 HARNESS_GDB_MODE=auto-continue` or `cargo xtask run --debug` (doc/book/debugging.md).
- Evidence before assertions — AGENTS.md §8. Reproduce, capture serial log / GDB backtrace, then propose fix.

## Execution strategy
### Parallel execution waves

| Wave | Todos | Rationale |
| --- | --- | --- |
| 1 | T1 | Allocator foundation — unblocks all multithread reliability work |
| 2 | T2, T3, T4, T5, T6, T13 | C1 remaining (depend on T1) + C2 (independent) + C6 discovery (independent) |
| 3 | T7, T8, T14, T15 | C3 (depends on C1) + C4 framework start + C6 S5 wiring (depends on T13) + C7 parser start |
| 4 | T9, T10, T16 | C4 IRQ+DMA (depend on T8) + C7 ld.so (depends on T15) |
| 5 | T11, T17 | C5 xHCI (depends on T8) + C7 dynamic TLS (depends on T16) |
| 6 | T12 | C5 USB HID (depends on T11) |

### Dependency matrix
| Todo | Depends on | Blocks | Can parallelize with |
| --- | --- | --- | --- |
| T1 | — | T2,T3,T4,T7 | — |
| T2 | T1 | T7 | T3,T4,T5,T6,T13 |
| T3 | T1 | — | T2,T4,T5,T6,T13 |
| T4 | T1 | — | T2,T3,T5,T6,T13 |
| T5 | — | — | T2,T3,T4,T6,T13 |
| T6 | T5 (same component, docs after code) | — | T13 |
| T7 | T2 (needs per-thread errno + reliable alloc) | — | T8,T14,T15 |
| T8 | — | T9,T10,T11 | T7,T14,T15 |
| T9 | T8 | — | T10,T16 |
| T10 | T8 | — | T9,T16 |
| T11 | T8 | T12 | T17 |
| T12 | T11 | — | — |
| T13 | — | T14 | T2,T3,T4,T5,T6 |
| T14 | T13 | — | T7,T8,T15 |
| T15 | — | T16 | T7,T8,T14 |
| T16 | T15 | T17 | T9,T10 |
| T17 | T16 | — | T11 |

## Todos
> Implementation + Test = ONE todo. Never separate.
<!-- APPEND TASK BATCHES BELOW THIS LINE WITH edit/apply_patch - never rewrite the headers above. -->

- [ ] 1. Allocator: blocking lock in `alloc` (dealloc unchanged)
  What to do / Must NOT do:
  - Change `LockedAllocator::alloc` (`userspace/libcluu/src/allocator.rs:769`) from `try_lock()` to blocking `lock()`. On contention, block until mutex acquired, then drain deferred-free queue, then proceed with allocation.
  - **MUST NOT** change `dealloc` (`allocator.rs:781`) — it keeps `try_lock` + deferred-free push. Changing `dealloc` to blocking would reintroduce re-entrant deadlock when a `Drop` fires mid-`alloc` (GC callback scenario, gotchas.md:30-36).
  - **MUST NOT** reimplement the deferred-free mechanism — it already exists (`DeferredFreeList` at allocator.rs:670-684, `drain_deferred` at 720-729, cap=64 at allocator.rs:793).
  - `alloc` never re-enters `alloc` (grow path calls `space_map_range` syscall only, allocator.rs:553-566); OOM handler runs after `drop(guard)` (allocator.rs:765-767). So `alloc` can safely block.
  - Update module doc (allocator.rs:36-38) to reflect: alloc blocks, dealloc defers.
  Parallelization: Wave 1 | Blocked by: — | Blocks: T2,T3,T4,T7
  References (executor has NO interview context - be exhaustive):
  - `userspace/libcluu/src/allocator.rs:36-38` — module doc (update)
  - `userspace/libcluu/src/allocator.rs:553-566` — grow path (space_map_range syscall, no re-entrancy)
  - `userspace/libcluu/src/allocator.rs:670-684` — DeferredFreeList ring buffer
  - `userspace/libcluu/src/allocator.rs:720-729` — drain_deferred
  - `userspace/libcluu/src/allocator.rs:765-767` — OOM handler (runs after guard drop)
  - `userspace/libcluu/src/allocator.rs:769-777` — alloc with try_lock (CHANGE to lock())
  - `userspace/libcluu/src/allocator.rs:780-796` — dealloc with try_lock+defer (DO NOT CHANGE)
  - `doc/book/gotchas.md:30-36` — re-entrant deadlock rationale
  Acceptance criteria (agent-executable): `cargo xtask build` succeeds. Run existing `pthreadprobe` probe — no OOM, no deadlock. Run MicroPython with GC-heavy script (allocate 10K objects, trigger `gc.collect()`, free all) — no deadlock, no OOM marker.
  QA scenarios (name the exact tool + invocation): `scripts/harness_run.sh` with `TEST_COMMAND` running pthreadprobe + micropython GC stress. Expect `ALLOC_OK` marker (new) and no `OOM`/`DEADLOCK` markers. Evidence `.omo/evidence/task-1-reliable-single-cpu-os.log`
  Commit: Y | fix(allocator): block in alloc instead of silent OOM on contention

- [ ] 2. Per-thread errno via FS:8 keying
  What to do / Must NOT do:
  - Change `errno_key()` (`userspace/libcluu/src/errno.rs:104-107`) to return `pthread_self()` instead of `token_self()`. `pthread_self()` reads FS:8 per-thread token (`userspace/libcluu/src/posix/pthread.rs:615-627`) with fallback to `token_self()` when FS:8 is 0 (main thread before `init_tls`).
  - The simplest correct change: `fn errno_key() -> usize { crate::posix::pthread::pthread_self() }` — `pthread_self()` already has the FS:8 read + fallback.
  - **MUST NOT** use `pid()` (wrong — same for all threads in a process). **MUST NOT** use `thread_get_id()` (extra syscall per errno access — wasteful when FS:8 already has it). **MUST NOT** create a new thread-local variable (redundant when FS:8 already holds the thread token). (Metis GAP-6/23)
  - Cleanup paths already assume per-thread keying but are currently dead: `pthread_entry` removes by `child_token` (pthread.rs:322-327), `pthread_exit` by `pthread_self()` (pthread.rs:1080-1084). This fix makes them effective.
  - Subtlety: allocator init runs before `init_tls()` (posix/mod.rs:105-112); FS:8=0 → `pthread_self()` falls back to `token_self()` = safe. Add a comment documenting this.
  Parallelization: Wave 2 | Blocked by: T1 | Blocks: T7
  References:
  - `userspace/libcluu/src/errno.rs:101-107` — errno_key() (CHANGE)
  - `userspace/libcluu/src/errno.rs:110-116,138-144,182-193` — set_errno, __errno, return_error sites
  - `userspace/libcluu/src/posix/pthread.rs:615-627` — pthread_self() (FS:8 read + fallback)
  - `userspace/libcluu/src/posix/pthread.rs:322-327` — pthread_entry cleanup (child_token)
  - `userspace/libcluu/src/posix/pthread.rs:1080-1084` — pthread_exit cleanup (pthread_self)
  - `userspace/libcluu/src/posix/mod.rs:105-112` — init order (allocator before init_tls)
  - `userspace/libcluu/src/posix/mod.rs:130-136` — init_tls sets FS:8
  - `userspace/libcluu/src/boot.rs:300-302` — token_self() (process token, same for all threads)
  Acceptance criteria: New probe `errno_probe` spawns 2 pthreads, each sets a distinct errno (e.g., 11 and 12), reads it back, prints `ERRNO_OK` if both match their own. Harness marker: `errno_probe_ok`. `cargo xtask build` succeeds.
  QA scenarios: `scripts/harness_run.sh` with `TEST_COMMAND` running `errno_probe`, `MARKER_MODE` expecting `ERRNO_OK`. Both threads must see their own errno. Evidence `.omo/evidence/task-2-reliable-single-cpu-os.log`
  Commit: Y | fix(pthread): key errno by per-thread FS:8 token, not process token

- [ ] 3. Honor `pthread_attr_setstacksize` in `pthread_create`
  What to do / Must NOT do:
  - `pthread_create` hardcodes `alloc_thread_stack(DEFAULT_STACK_PAGES)` (pthread.rs:366), ignores the `_attr` parameter. Fix: derive stack size from attr, compute pages, thread through lines 366/375/384/432.
  - `pthread_attr_t = usize` (pthread.rs:46), opaque C struct. `setstacksize` stores bytes into `*attr` (pthread.rs:657-666); `attr_init` writes 0 sentinel (pthread.rs:640-648).
  - Code shape:
    ```rust
    let stack_size = if !_attr.is_null() {
        let sz = unsafe { *_attr };
        if sz == 0 { DEFAULT_STACK_SIZE } else { sz }
    } else { DEFAULT_STACK_SIZE };
    let stack_pages = (stack_size + PAGE_SIZE - 1) / PAGE_SIZE;
    ```
    Store `stack_pages * PAGE_SIZE` into `PthreadInternal.stack_size` (consumed by join at pthread.rs:526).
  - `DEFAULT_STACK_PAGES=16` (pthread.rs:108), `DEFAULT_STACK_SIZE = DEFAULT_STACK_PAGES * PAGE_SIZE`. `alloc_thread_stack(n)` already takes page count (pthread.rs:231-251) — no change there.
  - **MUST NOT** change `alloc_thread_stack` signature — it already accepts page count.
  Parallelization: Wave 2 | Blocked by: T1 | Blocks: —
  References:
  - `userspace/libcluu/src/posix/pthread.rs:46` — pthread_attr_t = usize
  - `userspace/libcluu/src/posix/pthread.rs:73-82` — PthreadInternal struct (stack_size field)
  - `userspace/libcluu/src/posix/pthread.rs:108` — DEFAULT_STACK_PAGES=16
  - `userspace/libcluu/src/posix/pthread.rs:231-251` — alloc_thread_stack(n pages)
  - `userspace/libcluu/src/posix/pthread.rs:366` — hardcoded alloc (CHANGE)
  - `userspace/libcluu/src/posix/pthread.rs:375,384,432` — stack_pages usage sites
  - `userspace/libcluu/src/posix/pthread.rs:526` — join consumes stack_size
  - `userspace/libcluu/src/posix/pthread.rs:640-648` — attr_init (0 sentinel)
  - `userspace/libcluu/src/posix/pthread.rs:657-666` — setstacksize (stores bytes)
  Acceptance criteria: New probe `stack_probe` calls `pthread_attr_setstacksize(32768)`, creates a thread, thread uses >16KB of stack (deep buffer), prints `STACK_OK`. `cargo xtask build` succeeds.
  QA scenarios: `scripts/harness_run.sh` with `TEST_COMMAND` running `stack_probe`, expecting `STACK_OK`. Evidence `.omo/evidence/task-3-reliable-single-cpu-os.log`
  Commit: Y | fix(pthread): honor pthread_attr_setstacksize instead of hardcoded 16 pages

- [ ] 4. Detached thread stack reclamation
  What to do / Must NOT do:
  - Currently `pthread_detach` exited-branch leaks stack (note at pthread.rs:600). TLS IS already freed (pthread.rs:593-598) — only STACK leaks. (Metis GAP-9)
  - Three parts:
    1. `pthread_detach` exited-branch: add missing `space_unmap(stack_base, stack_size/PAGE_SIZE)` (pthread.rs:593-604). Safe — runs on detacher thread.
    2. Detached self-exit: add `REAP_QUEUE: Mutex<Vec<ReapEntry>>` where `ReapEntry = { stack_base, stack_size, tls_block, box_ptr }`. Exit path pushes entry. `reap_dead_threads()` called at top of `pthread_create` drains the queue.
    3. Race close: add `cleanup_claimed: AtomicU32` to `PthreadInternal` (pthread.rs:73-82). Gate every reclaim site (join, detach, reap) with `compare_exchange(0, 1)` to prevent double-free.
  - Model to mirror: `pthread_join` reclaims on the joiner thread (pthread.rs:523-552): unmap stack, dealloc TLS, drop Box, `thread_destroy`.
  - **Hazard**: a detached thread can't free its own stack/TLS inline (running on them, FS points in). That's why REAP_QUEUE + deferred drain is needed.
  - **MUST NOT** free TLS again — it's already freed at pthread.rs:593-598. Only stack leaks.
  Parallelization: Wave 2 | Blocked by: T1 | Blocks: —
  References:
  - `userspace/libcluu/src/posix/pthread.rs:73-82` — PthreadInternal struct (add cleanup_claimed)
  - `userspace/libcluu/src/posix/pthread.rs:280-336` — pthread_entry completion path (add REAP_QUEUE push)
  - `userspace/libcluu/src/posix/pthread.rs:523-552` — pthread_join reclaim model
  - `userspace/libcluu/src/posix/pthread.rs:593-604` — pthread_detach exited-branch (add space_unmap)
  - `userspace/libcluu/src/posix/pthread.rs:600` — "stack leak for detached threads" comment
  - `userspace/libcluu/src/posix/pthread.rs:1080-1084` — pthread_exit cleanup
  Acceptance criteria: New probe: 1000 detach loops, confirm no page-usage growth. Run `cat /proc/meminfo` before and after via shell. Harness marker: `DETACH_OK`. `cargo xtask build` succeeds.
  QA scenarios: `scripts/harness_run.sh` with `TEST_COMMAND` running detached-thread loop probe, expecting `DETACH_OK` and stable meminfo. Evidence `.omo/evidence/task-4-reliable-single-cpu-os.log`
  Commit: Y | fix(pthread): reclaim detached thread stacks via reap queue + race guard

- [ ] 5. mprotect(PROT_NONE) — kernel + userspace
  What to do / Must NOT do:
  - **Userspace** (`userspace/libcluu/src/posix/memory.rs:610-614`): delete the `PROT_NONE → ENOSYS` early-return block. `kern_flags` is already 0 for PROT_NONE, falls through to line 622 which calls `space_protect`.
  - **Kernel** (`kernel/src/syscall/handlers.rs:1805-1807`): `invoke_space_protect` hardcodes `present: true, user: true`. With perms=0, page is readonly-noexec but still READABLE (present=1 on x86 = accessible). Fix: derive `user` bit from access perms:
    ```rust
    let any_access = readable || writable || executable;
    let flags = PageFlags { present: true, writable, user: any_access, no_execute: !executable, .. };
    ```
  - **Why `user:false` instead of `present:false`**: mmap region `0x4100_0000..0x5000_0000` is NOT demand-paged (idt.rs:962-964), so a fault there is a real fault. Keeping `present:true` lets a later `mprotect(PROT_READ)` restore via `update_flags` (vmm.rs:463-476) without a new flag/op. `user:false` causes a #PF on user-mode access — true no-access. perms==0 already passes validation (handlers.rs:1651). (Opus analysis C2#5)
  - Path: `memory.rs:622` → `syscall.rs:958` `invoke(InvokeOp::SpaceProtect)` → `handlers.rs:791` → `invoke_space_protect:1634` → `vmm.protect:1819`. Op 16 defined at `token/mod.rs:388`.
  - **MUST NOT** add a new InvokeOp or syscall — `SpaceProtect=16` already handles this.
  - **MUST NOT** set `present:false` — breaks restorability via `update_flags`.
  Parallelization: Wave 2 | Blocked by: — | Blocks: —
  References:
  - `userspace/libcluu/src/posix/memory.rs:610-614` — PROT_NONE early-return (DELETE)
  - `userspace/libcluu/src/posix/memory.rs:622` — space_protect call (already wired)
  - `userspace/libcluu/src/syscall.rs:958` — invoke(InvokeOp::SpaceProtect)
  - `kernel/src/syscall/handlers.rs:791` — dispatch
  - `kernel/src/syscall/handlers.rs:1634-1679` — invoke_space_protect
  - `kernel/src/syscall/handlers.rs:1651` — perms validation (perms==0 passes)
  - `kernel/src/syscall/handlers.rs:1805-1807` — PageFlags hardcode (CHANGE user bit)
  - `kernel/src/syscall/handlers.rs:1819` — vmm.protect call
  - `kernel/src/token/mod.rs:388` — SpaceProtect=16
  - `kernel/src/architecture/x86_64/idt.rs:962-964` — mmap region not demand-paged
  - `kernel/src/mm/vmm.rs:463-476` — update_flags (restorability path)
  - `doc/book/interpreter_porting.md:74-82` — consumer (generational-GC write-barrier)
  Acceptance criteria: New probe: mmap a page, write sentinel, `mprotect(PROT_NONE)`, attempt read → kernel page fault → thread killed with fault marker on serial. Then `mprotect(PROT_READ)`, read → OK. Regression: R/W mprotect still returns 0. `cargo xtask build` succeeds.
  QA scenarios: `scripts/harness_run.sh` with `TEST_COMMAND` running mprotect probe, expecting fault marker on PROT_NONE access + `MPROTECT_RESTORE_OK` on re-read. Evidence `.omo/evidence/task-5-reliable-single-cpu-os.log`
  Commit: Y | fix(mm): implement true PROT_NONE via user:false PTE flag

- [ ] 6. Doc staleness fixes (3 documents)
  What to do / Must NOT do:
  - **A — `doc/book/interpreter_porting.md:46,91-93`**: says "Stack: 64 KB, fixed, no growth" — WRONG. Real: `USER_STACK_SIZE = 16 MiB` (kernel/src/mm/space.rs:53), demand-paged (idt.rs:1013-1041), proven by `probes/stackgrow/src/main.rs`. `memory_model.md:89-94` states it right. → Rewrite to "64 KiB mapped, demand-grows to 16 MiB ceiling."
  - **B — `README.md:25,35`**: says "no threading" — WRONG. `mpconfigport.h:33-34` sets `MICROPY_PY_THREAD (1)` + GIL. → Remove "no threading" (sockets stay off, :40, correctly).
  - **C — `doc/book/gotchas.md:13-23,51-57`**: frames deferred-free as planned/future with stale line refs — it LANDED (allocator.rs:660-796, `DeferredFreeList`+`drain_deferred`). → Mark "LANDED", fix refs. Note residual: 64-entry queue overflow still leaks (allocator.rs:793).
  - **D — `doc/book/memory_model.md:98`** (Metis GAP-26): documents "64 KiB (16 pages, DEFAULT_STACK_PAGES = 16)" as pthread stack size. After T3 makes stack size configurable, update to note that `pthread_attr_setstacksize` is now honored; default remains 64 KiB.
  - **MUST NOT** rewrite docs beyond the specific stale claims. Minimal edits only.
  Parallelization: Wave 2 | Blocked by: T5 (same component, docs after code) | Blocks: —
  References:
  - `doc/book/interpreter_porting.md:46,91-93` — stale stack claim
  - `doc/book/interpreter_porting.md:74-82` — write-barrier consumer reference
  - `README.md:25,35,40` — stale threading claim
  - `doc/book/gotchas.md:13-23,51-57` — stale deferred-free claim
  - `doc/book/memory_model.md:89-94,98` — correct stack info + stale pthread stack size
  - `userspace/micropython/mpconfigport.h:33-34` — MICROPY_PY_THREAD=1
  - `userspace/libcluu/src/allocator.rs:660-796` — deferred-free LANDED
  - `userspace/libcluu/src/posix/pthread.rs:108` — DEFAULT_STACK_PAGES=16
  - `kernel/src/mm/space.rs:53` — USER_STACK_SIZE = 16 MiB
  - `kernel/src/architecture/x86_64/idt.rs:1013-1041` — demand paging
  - `probes/stackgrow/src/main.rs` — stack growth proof
  Acceptance criteria: `grep -n "64 KB.*fixed\|no growth\|no threading\|planned.*deferred\|Add a deferred-free" doc/book/interpreter_porting.md README.md doc/book/gotchas.md` returns no matches. Doc build (if any) succeeds.
  QA scenarios: Manual verification — read each changed section, confirm accuracy against code. Evidence `.omo/evidence/task-6-reliable-single-cpu-os.diff`
  Commit: Y | docs: fix stale stack-size, threading, and deferred-free claims

- [ ] 7. MicroPython cross-thread GC stack scanning
  What to do / Must NOT do:
  - `mp_thread_gc_others` (`userspace/micropython/mpthreadport.c:224-234`) walks the thread list but calls ZERO `gc_collect_root` — even `th->arg` (a real root) is dropped. Objects live only on peer stacks get swept → UAF.
  - CLUU has no way to read a peer's live SP/registers from userspace (no InvokeOp; unix-port's signal-into-peer trick unavailable — no cross-thread `pthread_kill`). Use the **esp32 port's conservative whole-stack scan** (`ports/esp32/mpthreadport.c`): scan the entire mapped stack region. Over-scan is safe for conservative GC.
  - Fixes:
    1. New libcluu C-ABI helpers in `userspace/libcluu/src/posix/pthread.rs`, re-exported via `userspace/libcluu_syscalls/src/lib.rs:101`:
       ```rust
       #[no_mangle] pub extern "C" fn cluu_thread_stack_region(tid: usize, base: *mut usize, size: *mut usize) -> c_int
       #[no_mangle] pub extern "C" fn cluu_thread_suspend(tid: usize) -> c_int
       #[no_mangle] pub extern "C" fn cluu_thread_resume(tid: usize) -> c_int
       ```
       `thread_suspend`/`thread_resume` exist as syscalls (libcluu/src/syscall.rs:1283-1300, ops 2/3).
    2. Add `stack_start`/`stack_len` to `mp_thread_t` (mpthreadport.c:15-22); populate main in `mp_thread_init`, children in `mp_thread_create`.
    3. Rewrite `mp_thread_gc_others`: hold recursive GIL, per node `gc_collect_root(&th->arg, 1)`, skip self + `!ready`, `suspend` peer, `gc_collect_root(th->stack_start, th->stack_len)`, `resume`.
  - Per-thread stack region known: `PthreadInternal{stack_base, stack_size}` (posix/pthread.rs:73-82) in `THREADS` map. Main thread stack = fixed `0x7F000000..0x80000000` (crt0.S:16-18).
  - GIL on (mpconfigport.h:34) but insufficient alone (peer in GIL-released C section).
  - **Residual gap (document in comment)**: callee-saved regs of off-CPU peers not scanned (rely on spill-at-blocking-call). Fully closing needs a "read thread context" op = new verb, out of scope.
  - Trace: `gc_collect` (gccollect.c:8) → `gc_collect_start` → self scan → `mp_thread_gc_others` → `gc_collect_end`. Single chokepoint.
  - **MUST NOT** add a new syscall for reading peer registers — out of scope, documented as residual.
  Parallelization: Wave 3 | Blocked by: T2 (needs per-thread errno + reliable alloc) | Blocks: —
  References:
  - `userspace/micropython/mpthreadport.c:15-22` — mp_thread_t struct (add stack_start/stack_len)
  - `userspace/micropython/mpthreadport.c:224-234` — mp_thread_gc_others stub (REWRITE)
  - `userspace/micropython/gccollect.c:8-15` — gc_collect entry
  - `userspace/micropython/gccollect.c` — gc_helper_collect_regs_and_stack
  - `userspace/libcluu/src/posix/pthread.rs:73-82` — PthreadInternal (stack_base, stack_size)
  - `userspace/libcluu/src/posix/pthread.rs:615-627` — pthread_self() (tid for lookup)
  - `userspace/libcluu/src/syscall.rs:1283-1300` — thread_suspend/thread_resume (ops 2/3)
  - `userspace/libcluu_syscalls/src/lib.rs:101` — C-ABI re-export point
  - `userspace/micropython/mpconfigport.h:33-34` — MICROPY_PY_THREAD=1, GIL on
  - `userspace/libcluu/src/crt0.S:16-18` — main thread stack range
  Acceptance criteria: N-worker `.py` stress — each worker builds object graphs referenced only on its stack, main forces `gc.collect()`, worker asserts payload intact AFTER the collect (UAF trap). New harness `MarkerModeSpec` requiring `C3_GC_OTHERS_OK` (`python/cluu_harness/markers.py`). Baseline flaky-fail, fixed deterministic-pass. `cargo xtask build` succeeds.
  QA scenarios: `scripts/harness_run.sh` with `TEST_COMMAND` running MicroPython GC stress script, `MARKER_MODE` expecting `C3_GC_OTHERS_OK`. Evidence `.omo/evidence/task-7-reliable-single-cpu-os.log`
  Commit: Y | feat(micropython): implement cross-thread GC stack scanning

- [ ] 8. `driver-framework` crate — BusDriver/DeviceDriver/IrqHandler traits
  What to do / Must NOT do:
  - New crate `userspace/driver-framework/` mirroring `virtio-core` layout (lib.rs:1-17). Files: `bus.rs`, `device.rs`, `irq.rs`, `dma.rs`, `mmio.rs`.
  - Register in root `Cargo.toml` `members` (~:6/83) + `default-members`.
  - Traits:
    ```rust
    trait BusDriver { fn enumerate(&self, pci_token) -> Result<Vec<Handle>>; fn bar(&self, ...) -> Option<(u64,u32)>; fn irq_line(&self, ...) -> Option<u8>; }
    trait DeviceDriver { fn class(&self) -> DeviceClass; fn init(&mut self, res) -> Result<()>; fn handle_message(&self, ...) -> Result<Reply>; }
    trait IrqHandler { fn on_irq(&mut self, irq: usize); }
    ```
  - devmgr is a passive registry/broker (not a PCI enumerator): recv loop at `userspace/devmgr/src/main.rs:65-118`; labels `REGISTER`/`REGISTER_CHAR`/`GRANT_REGION`/`GRANT_DEVICE`/`REVOKE`/`LIST_FOR_ENVELOPE`. Model = `BTreeMap<DeviceId,DeviceEntry>` (`userspace/devmgr/src/dev_registry.rs:29`). Drivers **self-register** (virtio-blk/src/main.rs:324).
  - devmgr stays sync — IRQ notifications BYPASS devmgr (kernel dispatches directly to driver endpoint via `devices::irq::dispatch`). devmgr never relays IRQs. (Metis GAP-15, Opus C4#8)
  - **MUST NOT** add `RequestIrq` or `MapDeviceRegion` to InvokeOp — `IrqAttach=30` (token/mod.rs:400) and `SpaceMap+MAP_DEVICE=0x100` (syscall.rs:136) already exist. (Metis GAP-1/2/22)
  - **MUST NOT** make devmgr async — it's a leaf registry, not an IPC relay.
  Parallelization: Wave 3 | Blocked by: — | Blocks: T9, T10, T11
  References:
  - `userspace/virtio-core/src/lib.rs:1-17` — crate layout to mirror
  - `userspace/virtio-core/src/dma.rs:16-102` — DmaPool (to be extracted in T10)
  - `userspace/devmgr/src/main.rs:65-118` — devmgr recv loop
  - `userspace/devmgr/src/dev_registry.rs:29` — DeviceEntry/BTreeMap model
  - `userspace/virtio-blk/src/main.rs:98-310` — driver recv loop pattern
  - `userspace/virtio-blk/src/main.rs:324` — self-register with devmgr
  - `kernel/src/token/mod.rs:400` — IrqAttach=30 (REUSE, do not duplicate)
  - `userspace/libcluu/src/syscall.rs:136` — MAP_DEVICE=0x100 (REUSE)
  - `Cargo.toml:6,83` — workspace members
  Acceptance criteria: `cargo xtask build` succeeds. New `driver-framework` crate compiles. Existing virtio-blk still builds (not yet migrated — that's fine). Unit test: minimal `dummy_driver` implements DeviceDriver trait, registers with devmgr, prints `DRIVER_OK`. Harness marker: `driver_framework_ok`. (Metis GAP-19)
  QA scenarios: `scripts/harness_run.sh` with `TEST_COMMAND` running dummy_driver probe, expecting `DRIVER_OK`. Evidence `.omo/evidence/task-8-reliable-single-cpu-os.log`
  Commit: Y | feat(driver-framework): add BusDriver/DeviceDriver/IrqHandler trait crate

- [ ] 9. Dynamic IRQ trampoline — generalize `dispatch_scancode`
  What to do / Must NOT do:
  - IDT gates hardcoded (kernel/src/architecture/x86_64/idt.rs:123-135): IRQ0→timer, 1→kbd, 4/7→serial, 11→virtio-blk, 12→mouse. But endpoint routing below is already dynamic: `IRQ_ENDPOINTS: [AtomicU64;16]` (kernel/src/devices/irq.rs:34), `attach:40`, `dispatch_scancode:50-100` (builds `UserMessage`, `endpoint::try_send`, `wake_thread`). PIC 8259 only (pic.rs:60-169); no IO-APIC redirection (apic.rs).
  - Fix: one generic trampoline on vectors 32-47 that recovers vector → `dispatch_irq(vec-32, label, payload)` → EOI, replacing the 5 bespoke handlers. `irq_attach` already unmasks, so no per-driver boot edit.
  - Generalize `dispatch_scancode` into `dispatch_irq(irq: u8, label: u64, payload: &[u8])` that sends to the registered endpoint with a device-class label. Existing `dispatch_scancode` becomes a thin wrapper: `dispatch_irq(irq, KBD_RAW_LABEL, &[scancode])`.
  - **MUST NOT** change the kbd IDT handler's call signature — preserve `KBD_RAW_LABEL=0x600` (irq.rs:25) for kbd. (Metis GAP-8)
  - **MUST NOT** add a new InvokeOp — `IrqAttach=30` already handles registration + PIC unmask.
  - **MUST NOT** make IDT vectors "dynamic" — vectors 32-47 are the x86 hardware contract for IRQs 0-15. The dynamic part is endpoint routing (already done) + dispatch label (this task).
  Parallelization: Wave 4 | Blocked by: T8 | Blocks: —
  References:
  - `kernel/src/architecture/x86_64/idt.rs:123-135` — hardcoded IDT gates (REPLACE with trampoline)
  - `kernel/src/architecture/x86_64/idt.rs:1398-1406` — example IDT handler
  - `kernel/src/devices/irq.rs:25` — KBD_RAW_LABEL=0x600 (PRESERVE)
  - `kernel/src/devices/irq.rs:34` — IRQ_ENDPOINTS: [AtomicU64;16]
  - `kernel/src/devices/irq.rs:40` — attach()
  - `kernel/src/devices/irq.rs:50-100` — dispatch_scancode (GENERALIZE into dispatch_irq)
  - `kernel/src/devices/pic.rs:60-169` — PIC 8259
  - `kernel/src/syscall/handlers.rs:3023-3059` — invoke_irq_attach (already dynamic)
  - `kernel/src/token/mod.rs:400` — IrqAttach=30
  Acceptance criteria: `cargo xtask build` succeeds. Boot CLUU — kbd still works (KBD_RAW_LABEL preserved). virtio-blk still works (ext2 mounts, main.rs:257). Mouse still works. All baseline harness cases pass.
  QA scenarios: `scripts/harness_run.sh` — full baseline suite. Expect all existing markers pass. Evidence `.omo/evidence/task-9-reliable-single-cpu-os.log`
  Commit: Y | refactor(irq): generic trampoline for vectors 32-47, preserve kbd label

- [ ] 10. Extract DmaPool to shared `dma-core` crate
  What to do / Must NOT do:
  - `DmaPool` (`userspace/virtio-core/src/dma.rs:16-102`): `new:35`, `alloc:57` (aligned, never crosses 4KiB page), `phys_of:90`. Already generic over `space_token`.
  - Extract into new `userspace/dma-core/` crate. API stays the same: `alloc(size) -> (virt, phys)`, `phys_of(virt) -> phys`. Add `alloc_contiguous(pages)` guaranteeing physical contiguity (via `PmmAllocLarge`, token/mod.rs:421) for >4KiB buffers.
  - Add `trait DmaAllocator { alloc_coherent(len, align); phys_of; space_token }`.
  - virtio-blk switches to the shared crate (update Cargo.toml dependency).
  - **MUST NOT** add streaming DMA, scatter-gather lists, or coherent vs streaming distinction. (Metis GAP-12)
  - **MUST NOT** change the DmaPool public API signature — just move + extend.
  Parallelization: Wave 4 | Blocked by: T8 | Blocks: —
  References:
  - `userspace/virtio-core/src/dma.rs:16-102` — DmaPool (EXTRACT)
  - `userspace/virtio-core/src/dma.rs:35` — new()
  - `userspace/virtio-core/src/dma.rs:57` — alloc() (aligned, <4KiB)
  - `userspace/virtio-core/src/dma.rs:90` — phys_of()
  - `userspace/virtio-core/src/lib.rs:1-17` — crate layout
  - `userspace/virtio-core/src/virtqueue.rs` — DmaPool caller
  - `userspace/virtio-blk/src/request_queue.rs` — DmaPool caller
  - `userspace/virtio-blk/src/main.rs:102,211` — TOKEN_EXTRA_1/2 grants
  - `kernel/src/token/mod.rs:421` — PmmAllocLarge (for alloc_contiguous)
  - `Cargo.toml:6,83` — workspace members
  Acceptance criteria: `cargo xtask build` succeeds. virtio-blk boots, ext2 mounts (main.rs:257), disk I/O works. `alloc_contiguous` unit test: allocate 8 pages, verify physical addresses are contiguous.
  QA scenarios: `scripts/harness_run.sh` — baseline suite. Expect ext2 mount + disk I/O markers pass. Evidence `.omo/evidence/task-10-reliable-single-cpu-os.log`
  Commit: Y | refactor(dma): extract DmaPool to shared dma-core crate, add alloc_contiguous

- [ ] 11. `xhci-core` crate — PCI enum, controller reset, TRB rings
  What to do / Must NOT do:
  - New crate `userspace/xhci-core/` (mirror virtio two-crate split): `pci.rs`, `regs.rs`, `dma.rs`, `ring.rs`, `context.rs`, `controller.rs`, `irq.rs`.
  - Add to workspace `members`.
  - **Modify QEMU config**: `python/cluu_harness/qemu.py:177-197` + `xtask/src/main.rs:2033-2039` add `-device qemu-xhci,id=xhci -device usb-kbd,bus=xhci.0 -device usb-mouse,bus=xhci.0`. (Opus found NO USB in QEMU today.)
  - **Modify init services**: `userspace/init/src/services.rs` (`XhciUsb` ServiceKind + `XHCI_RIGHTS` mirroring `VIRTIOBLK_RIGHTS:66-77`), `userspace/init/src/wiring.rs` (grant PCI `child_token`+irq token, mirror `:189-196`), `userspace/init/src/context.rs` (irq token derive `:63-67`).
  - xHCI regs (all in BAR0, class 0x0C0330): cap regs (CAPLENGTH, DBOFF, RTSOFF, HCSPARAMS), op regs (USBCMD/USBSTS/CRCR/DCBAAP/CONFIG + port set at +0x400), runtime (interrupter IMAN/ERSTBA/ERDP), doorbell array. Use byte-offset volatile idiom (modern_pci.rs:50-77). TRB rings mirror `virtqueue.rs` producer/consumer+cycle-bit. DCBAA/contexts 64-byte aligned via `DmaPool` align arg. Legacy IRQ from PCI cfg 0x3C (virtio-blk/main.rs:198-217).
  - **Caveat**: autostart/Cluufile route grants only `IRQ_HANDLE`, not `PCI_ACCESS` (root-procmgr/main.rs:7555-7558). HC needs PCI config access → must spawn from init like virtio-blk (elevated `child_token`, wiring.rs:193).
  - **MUST NOT** implement USB hubs, multi-device, isochronous transfers, or USB 2/3 speed negotiation beyond QEMU defaults. (Metis GAP-10)
  - Scope IN: PCI enum of xHCI, halt/reset controller, set up single command ring + event ring, enumerate ONE device, address slot, configure endpoint.
  Parallelization: Wave 5 | Blocked by: T8 (framework) | Blocks: T12
  References:
  - `userspace/virtio-core/src/lib.rs:1-17` — crate layout to mirror
  - `userspace/virtio-core/src/pci.rs:75-110` — PCI config space scan pattern
  - `userspace/virtio-core/src/modern_pci.rs:50-77` — volatile MMIO byte-offset idiom
  - `userspace/virtio-core/src/virtqueue.rs` — TRB ring pattern (producer/consumer+cycle-bit)
  - `userspace/virtio-blk/src/main.rs:98-310` — driver recv loop pattern
  - `userspace/virtio-blk/src/main.rs:198-217` — legacy IRQ from PCI cfg 0x3C
  - `userspace/init/src/services.rs:66-77` — VIRTIOBLK_RIGHTS (mirror for XHCI_RIGHTS)
  - `userspace/init/src/wiring.rs:189-196` — PCI child_token grant pattern
  - `userspace/init/src/context.rs:63-67` — irq token derive
  - `python/cluu_harness/qemu.py:177-197` — QEMU args (ADD xHCI)
  - `xtask/src/main.rs:2033-2039` — QEMU config (ADD xHCI)
  - `userspace/root-procmgr/src/main.rs:7555-7558` — autostart grants (IRQ_HANDLE only)
  - `kernel/src/token/mod.rs:400` — IrqAttach=30 (for IRQ registration)
  - `userspace/libcluu/src/syscall.rs:136` — MAP_DEVICE=0x100 (for BAR mapping)
  Acceptance criteria: `cargo xtask build` succeeds. Boot CLUU with USB enabled. PCI discovery marker `XHCI_PCI_OK`. HC running (USBSTS.CNR clear) marker `XHCI_RESET_OK`. Port enable + slot address marker `XHCI_SLOT_OK`.
  QA scenarios: `scripts/harness_run.sh` with USB-enabled QEMU, `MARKER_MODE` expecting `XHCI_PCI_OK` + `XHCI_RESET_OK` + `XHCI_SLOT_OK`. Evidence `.omo/evidence/task-11-reliable-single-cpu-os.log`
  Commit: Y | feat(xhci): add xhci-core crate with PCI enum + controller reset + TRB rings

- [ ] 12. `usb-hid` crate — boot-protocol HID keyboard + mouse
  What to do / Must NOT do:
  - New crate `userspace/usb-hid/` (binary, main.rs recv loop mirror virtio-blk/main.rs:98-310).
  - Boot protocol: `SET_PROTOCOL(0)`, poll interrupt-IN, skip full descriptor parse.
  - Connects to existing input stack: feed `inputd:input` exactly as PS/2 `kbd`/`mouse` do.
  - Keyboard: `build_kbd_event(ascii, scancode, mods, ext)` (kbd/src/protocol.rs:37-51) → `KBD_EVENT_LABEL=1` (ipc.rs:26) → `send(inputd_input_ep)` (kbd/src/context.rs:157) → inputd decodes (inputd/src/main.rs:71-97) → vtmgr → tty. Mouse: `MOUSE_EVENT_LABEL=104`.
  - USB HID is *additive* alongside PS/2, same endpoint. Map USB 8-byte boot report [mods,resv,key0..5] + modifier bits → `MOD_SHIFT/CTRL/ALT` (kbd/src/protocol.rs:12-17).
  - **MUST NOT** implement full HID descriptor parsing — boot-protocol only. (Metis GAP-10)
  - **MUST NOT** replace PS/2 — additive alongside.
  Parallelization: Wave 6 | Blocked by: T11 | Blocks: —
  References:
  - `userspace/virtio-blk/src/main.rs:98-310` — driver recv loop pattern
  - `userspace/kbd/src/protocol.rs:12-17` — MOD_SHIFT/CTRL/ALT bits
  - `userspace/kbd/src/protocol.rs:37-51` — build_kbd_event()
  - `userspace/kbd/src/context.rs:15,157` — irq_attach + send to inputd
  - `userspace/kbd/src/ipc.rs:26` — KBD_EVENT_LABEL=1
  - `userspace/inputd/src/main.rs:71-97` — inputd decode
  - `userspace/mouse/src/main.rs` — mouse pattern (MOUSE_EVENT_LABEL=104)
  - `userspace/init/src/services.rs` — ServiceKind (add UsbHid)
  - `userspace/init/src/wiring.rs` — grant pattern
  Acceptance criteria: `cargo xtask build` succeeds. Boot CLUU with USB. End-to-end keystroke via harness `sendkey` (suite.py:131,158-170) → shell echo. Marker `USB_KBD_OK`. Mouse movement marker `USB_MOUSE_OK`. Regression: PS/2 still works.
  QA scenarios: `scripts/harness_run.sh` with USB-enabled QEMU, `SENDKEY_SEQUENCE` for keystroke, `MARKER_MODE` expecting `USB_KBD_OK`. Evidence `.omo/evidence/task-12-reliable-single-cpu-os.log`
  Commit: Y | feat(usb-hid): boot-protocol HID keyboard+mouse, additive to PS/2

- [ ] 13. `acpi` crate — RSDP discovery + table parsing
  What to do / Must NOT do:
  - New crate `userspace/acpi/` (mirror timeserver skeleton: `Cargo.toml` + `#![no_std]#![no_main]` `main()->i32` pulling tokens from `process_info().tokens[]`).
  - Files: `tables.rs` (`#[repr(C,packed)]` RSDP/SdtHeader/FADT/MCFG/GenericAddress + `checksum`), `discover.rs`, `power.rs`.
  - Phys map (already exists): `space_map_range(space, va, phys, MAP_DEVICE|0x01, pages, 0)` (modern_pci.rs:100; kernel treats data_ptr as phys base when MAP_DEVICE, handlers.rs:2255-2276; no phys-range restriction — low RAM/EBDA/0xE0000 all mappable). Reserve `ACPI_VA_BASE=0x5300_0000`.
  - RSDP scan: map page 0 (EBDA ptr at phys 0x40E) + `0xE0000..0x100000`, scan 16-byte boundaries for `b"RSD PTR "`, validate checksum. revision→RSDT(u32) vs XSDT(u64). Parse FADT `pm1a_cnt_blk`, MCFG `base_address` (ECAM). `slp_typ_a` lives in DSDT `\_S5_` AML — minimal ACPI hardcodes 0 (correct for QEMU; today's `0x2000` already assumes it).
  - **MUST NOT** implement AML interpreter or `_PRT` — QEMU uses static INTx lines from PCI config offset 0x3C. (Metis GAP-11)
  - **MUST NOT** parse DSDT — hardcode `slp_typ_a=0` (correct for QEMU).
  Parallelization: Wave 2 | Blocked by: — | Blocks: T14
  References:
  - `userspace/timeserver/` — crate skeleton to mirror
  - `userspace/virtio-core/src/modern_pci.rs:100` — space_map_range MAP_DEVICE pattern
  - `kernel/src/syscall/handlers.rs:2255-2276` — MAP_DEVICE phys mapping
  - `userspace/libcluu/src/syscall.rs:136` — MAP_DEVICE=0x100
  - `userspace/init/src/main.rs:111-112` — hardcoded S5 (0x604, 0x2000) — TO BE REPLACED by FADT-derived
  - `userspace/init/src/main.rs:118` — reboot via 0xCF9←0x06
  - `kernel/src/token/mod.rs:416` — PortOut16=56 (for PM1a write)
  - `kernel/src/syscall/handlers.rs:3309-3324` — invoke_port_out16 (Rights::PCI_ACCESS gated)
  - `userspace/libcluu/src/syscall.rs:1589` — port_out16 wrapper
  - `kernel/src/token/rights.rs:121` — PCI_ACCESS=1<<30
  - `kernel/src/token/rights.rs:86` — SPACE_MAP=1<<16
  - `Cargo.toml:6,83` — workspace members
  Acceptance criteria: `cargo xtask build` succeeds. Boot CLUU. Boot-time serial log: discovered RSDP rev, table sigs+checksums, FADT PM1a, MCFG ECAM. Markers `ACPI_RSDP_OK` + `ACPI_TABLES_OK`. Negative probe: service without PCI_ACCESS calling port_out16 → `PermissionDenied` (handlers.rs:3312).
  QA scenarios: `scripts/harness_run.sh` with `MARKER_MODE` expecting `ACPI_RSDP_OK` + `ACPI_TABLES_OK`. Evidence `.omo/evidence/task-13-reliable-single-cpu-os.log`
  Commit: Y | feat(acpi): RSDP discovery + FADT/MCFG parsing

- [ ] 14. Wire ACPI S5 from FADT + MCFG ECAM exposure
  What to do / Must NOT do:
  - Port I/O must live in a userspace `PCI_ACCESS` holder — userspace has no IOPL; all `in`/`out` proxied through `invoke_port_out16` gated on `Rights::PCI_ACCESS` (handlers.rs:3309-3324; op 56, token/mod.rs:416; wrapper syscall.rs:1589). NOT kernel — table parsing is device knowledge = userspace discipline.
  - Chain: kbd Ctrl+Alt+Del (kbd/main.rs:88) → `root-procmgr handle_shutdown` (main.rs:2214-2245) → `notify_exit(42)` → init monitors (init/main.rs:96) → S5 write.
  - FADT-derived S5:
    ```rust
    let val = ((slp_typ_a & 0x7) << 10) | (1 << 13); // SLP_EN
    port_out16(pci_token, pm1a_cnt_port, val); // QEMU → 0x604, 0x2000
    ```
  - Replace hardcoded `0x604`/`0x2000` at init/src/main.rs:111-112 with FADT-derived values from acpi service.
  - MCFG ECAM: expose `base_address` from MCFG for PCI config space access (future PCI enumeration via ECAM instead of config-space I/O ports).
  - Crate wiring: root `Cargo.toml` members; `userspace/init/src/services.rs` (`ACPI_RIGHTS = PCI_ACCESS|SPACE_MAP|IPC_*|CREATE`, `ServiceKind::Acpi`, `ServiceSpec`); `userspace/init/src/wiring.rs` Acpi arm (PCI `child_token`).
  - **MUST NOT** put port I/O in kernel — userspace discipline (AGENTS.md §2).
  - **QEMU runs `-no-reboot -no-shutdown`** (qemu.py:192-193) → S5 freezes guest, does NOT exit → verify by serial markers, not exit code. For real poweroff test, temporarily drop `-no-shutdown` and assert QEMU termination.
  Parallelization: Wave 3 | Blocked by: T13 | Blocks: —
  References:
  - `userspace/init/src/main.rs:96` — init monitors exit
  - `userspace/init/src/main.rs:111-112` — hardcoded S5 (REPLACE with FADT-derived)
  - `userspace/init/src/main.rs:118` — reboot via 0xCF9←0x06
  - `userspace/init/src/services.rs:66-77` — VIRTIOBLK_RIGHTS (mirror for ACPI_RIGHTS)
  - `userspace/init/src/wiring.rs:189-196` — grant pattern
  - `userspace/kbd/src/main.rs:88` — Ctrl+Alt+Del → shutdown
  - `userspace/root-procmgr/src/main.rs:2214-2245` — handle_shutdown
  - `kernel/src/syscall/handlers.rs:3309-3324` — invoke_port_out16 (PCI_ACCESS gated)
  - `kernel/src/syscall/handlers.rs:3312` — PermissionDenied on missing right
  - `userspace/libcluu/src/syscall.rs:1589` — port_out16 wrapper
  - `kernel/src/token/mod.rs:416` — PortOut16=56
  - `kernel/src/token/rights.rs:121` — PCI_ACCESS=1<<30
  - `python/cluu_harness/qemu.py:192-193` — -no-reboot -no-shutdown (verify by marker not exit)
  - `python/cluu_harness/markers.py:238` — l2_cluuterm_exit marker pattern
  Acceptance criteria: `cargo xtask build` succeeds. Boot CLUU. ACPI service logs FADT-derived PM1a port + slp_typ. Ctrl+Alt+Del → S5 write → serial marker `ACPI_S5_OK`. QEMU freezes (guest, not exit). For real poweroff: temporarily drop `-no-shutdown`, assert QEMU process exits.
  QA scenarios: `scripts/harness_run.sh` with `MARKER_MODE` expecting `ACPI_S5_OK`. For poweroff test: modified QEMU config without `-no-shutdown`, assert process termination. Evidence `.omo/evidence/task-14-reliable-single-cpu-os.log`
  Commit: Y | feat(acpi): FADT-derived S5 shutdown replacing hardcoded magic

- [ ] 15. `boot_elf.rs` ET_DYN acceptance + auxv infrastructure
  What to do / Must NOT do:
  - **Parser edits** (`klibcluu/src/boot_elf.rs`): accept ET_DYN (relax `:162-164` which hard-rejects non-ET_EXEC), capture PT_INTERP/PT_DYNAMIC/PT_TLS/PT_PHDR. Add `Elf64Rela`/`Dyn`/`Sym` structs + `R_X86_64_*`/`DT_*` consts.
  - **ET_DYN bias**: add `load_bias` param to `map_elf_segment` (vfs/main.rs:5203,5265), map at `vaddr+bias`. No syscall change — caller computes offset; `space_map_range` unchanged.
  - **VFS reports interp info**: `vfs.map_elf` also reports `interp_path`/`e_type`/`phdr_vaddr`/`phnum`/`entry`. If PT_INTERP present, procmgr maps ld.so instead, `thread_create` entry = ld.so entry (elf_spawn.rs:175).
  - **Auxv**: write auxv into ProcessInfo page (new `PARAM_AUXV_*`, mirror argv at elf_spawn.rs:446-459): AT_PHDR/PHENT/PHNUM/ENTRY/BASE/NULL. Currently no auxv exists (crt0.S:141 passes envp=NULL).
  - **Reloc engine lives in ld-cluu** (has alloc), not shared no_std parser (caps 8 segments, boot_elf.rs:10). Parser just captures PT_DYNAMIC location; ld-cluu processes it.
  - **MUST NOT** put the relocation engine in klibcluu — it's no_std with 8-segment cap, no alloc. Reloc engine needs alloc → lives in ld-cluu (T16).
  - **MUST NOT** change `space_map_range` syscall — caller computes bias offset.
  - Regression: static ET_EXEC still spawns (no PT_INTERP → bias=0, ld.so skipped).
  Parallelization: Wave 3 | Blocked by: — | Blocks: T16
  References:
  - `klibcluu/src/boot_elf.rs:10` — 8-segment cap (no alloc → reloc engine in ld-cluu)
  - `klibcluu/src/boot_elf.rs:162-164` — ET_EXEC hard-reject (RELAX to accept ET_DYN)
  - `klibcluu/src/boot_elf.rs:194` — PT_LOAD collection (ADD PT_INTERP/PT_DYNAMIC/PT_TLS/PT_PHDR capture)
  - `userspace/vfs/src/main.rs:5071` — handle_map_elf
  - `userspace/vfs/src/main.rs:5159` — ElfFile::parse (xmas-elf crate, accepts ET_DYN — doesn't check e_type)
  - `userspace/vfs/src/main.rs:5184,5203,5265` — map_elf_segments (ADD load_bias param)
  - `userspace/session-procmgr/src/elf_spawn.rs:127` — begin_spawn
  - `userspace/session-procmgr/src/elf_spawn.rs:175` — thread_create entry (set to ld.so entry if PT_INTERP)
  - `userspace/session-procmgr/src/elf_spawn.rs:446-459` — argv pattern (MIRROR for auxv)
  - `userspace/libcluu/src/crt0.S:141` — envp=NULL (no auxv currently)
  - `userspace/libcluu/src/posix/pthread.rs:151-215` — static TLS variant II
  Acceptance criteria: `cargo xtask build` succeeds. Parser accepts ET_DYN ELF (unit test: parse valid ET_DYN, no error). PIE self-reloc no-libs test: trivial ET_DYN binary with no DT_NEEDED loads and runs, prints `PIE_OK`. Regression: static ET_EXEC still spawns (no PT_INTERP → bias=0, ld.so skipped).
  QA scenarios: `scripts/harness_run.sh` with `TEST_COMMAND` running PIE self-reloc probe, expecting `PIE_OK`. Evidence `.omo/evidence/task-15-reliable-single-cpu-os.log`
  Commit: Y | feat(elf): accept ET_DYN + auxv infrastructure for dynamic linking

- [ ] 16. `ld-cluu` crate — self-reloc + DT_NEEDED + reloc engine
  What to do / Must NOT do:
  - New crate `userspace/ld-cluu/` (ET_DYN PIE, self-relocating) + PIE linker script.
  - **Design B** (keeps VFS dumb): ld.so self-relocates (RELATIVE only), walks DT_NEEDED, maps libs at fresh bias, applies relocs BIND_NOW, sets up TLS, jumps to AT_ENTRY (app's `_start`, crt0 as today).
  - Reloc loop [SPEC]: RELATIVE (`*w = base+addend`), 64 (`S+A`), GLOB_DAT/JUMP_SLOT (`S`), DTPMOD64/DTPOFF64/TPOFF64. `resolve()` walks global symbol order via DT_HASH/GNU_HASH.
  - Eager resolution (BIND_NOW) — no lazy PLT trampoline. Still requires full relocation processor (DT_STRTAB, DT_SYMTAB, R_X86_64_* reloc types). The "simpler" claim vs lazy refers only to avoiding PLT trampolines, not the reloc engine. (Metis GAP-16)
  - **MUST NOT** implement lazy PLT resolution — BIND_NOW only. No signal-based lazy trap (CLUU has no async signal delivery).
  - **MUST NOT** put reloc engine in klibcluu — it's no_std with no alloc. ld-cluu has alloc.
  Parallelization: Wave 4 | Blocked by: T15 | Blocks: T17
  References:
  - `klibcluu/src/boot_elf.rs` — shared parser (ET_DYN now accepted from T15)
  - `userspace/vfs/src/main.rs:5071,5159,5184,5203,5265` — VFS map_elf (reports interp path from T15)
  - `userspace/session-procmgr/src/elf_spawn.rs:127,175,446-459` — procmgr spawn (maps ld.so, writes auxv from T15)
  - `userspace/libcluu/src/crt0.S:141` — crt0 entry
  - `userspace/libcluu/src/posix/pthread.rs:151-215` — static TLS variant II (FS base)
  - `userspace/libcluu/src/posix/mod.rs:130` — ThreadSetFSBase
  Acceptance criteria: `cargo xtask build` succeeds. Trivial `libgreet.so` (ET_DYN, `greet()→0x42`) + `dyntest` (PT_INTERP=/lib/ld-cluu.so) prints `DYN_OK 42`. Harness marker: `dyn_link_ok`. Verify: `readelf -h libgreet.so` shows `Type: DYN`, `dyn_test` runs and exits 0. (Metis GAP-20)
  Milestones each harness-checkable: parser accepts ET_DYN → PIE self-reloc no-libs → one DT_NEEDED lib → dynamic `__thread`.
  QA scenarios: `scripts/harness_run.sh` with `TEST_COMMAND` running `dyntest`, `MARKER_MODE` expecting `DYN_OK 42`. Evidence `.omo/evidence/task-16-reliable-single-cpu-os.log`
  Commit: Y | feat(ld-cluu): userspace dynamic linker with eager relocation

- [ ] 17. Dynamic TLS — `__tls_get_addr` + DTV
  What to do / Must NOT do:
  - Static exe TLS stays module 1 (negative FS offset, pthread.rs:192). Add DTV + `__tls_get_addr(TlsIndex{ti_module,ti_offset})` reachable from TCB.
  - Each dlopen'd lib gets module id + DTV slot. TPOFF64 needs negative offset vs final static TLS size (interacts with `tls_aligned` pthread.rs:178).
  - `__tls_get_addr` is the dynamic TLS accessor: if DTV[module] not allocated, allocate the TLS block, set DTV[module] = pointer, return `ptr + offset`.
  - **MUST NOT** change static TLS layout for existing ET_EXEC binaries — module 1 stays negative FS offset.
  Parallelization: Wave 5 | Blocked by: T16 | Blocks: —
  References:
  - `userspace/libcluu/src/posix/pthread.rs:151-215` — static TLS variant II (FS base)
  - `userspace/libcluu/src/posix/pthread.rs:178` — tls_aligned
  - `userspace/libcluu/src/posix/pthread.rs:192` — module 1 negative FS offset
  - `userspace/libcluu/src/posix/mod.rs:130` — ThreadSetFSBase
  - `userspace/ld-cluu/` — ld.so (from T16, where __tls_get_addr lives)
  Acceptance criteria: `cargo xtask build` succeeds. Test: `libthread.so` (ET_DYN, exports `__thread int counter`), `tls_test` (PT_INTERP, links libthread.so, reads/writes `__thread` var, prints `DYN_TLS_OK`). Harness marker: `dyn_tls_ok`.
  QA scenarios: `scripts/harness_run.sh` with `TEST_COMMAND` running `tls_test`, `MARKER_MODE` expecting `DYN_TLS_OK`. Evidence `.omo/evidence/task-17-reliable-single-cpu-os.log`
  Commit: Y | feat(tls): dynamic TLS via __tls_get_addr + DTV for shared libs

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for the user's explicit okay before declaring complete.
- [ ] F1. Plan compliance audit — verify every todo matches its spec, zero new syscalls/InvokeOps, no runtime ACL, AGENTS.md §2-§6 respected
- [ ] F2. Code quality review — no `as any`/`unwrap`/`@ts-ignore`, `Result<T>` over panics, `no_std`+`alloc` explicit, `debug_print` for serial
- [ ] F3. Real manual QA — full harness suite (`python3 -m cluu_harness`), all baseline + new markers pass, GDB for any hangs
- [ ] F4. Scope fidelity — no scope creep beyond Must have, all Must NOT have respected, docs updated

## Commit strategy
- One commit per todo (17 commits + any fixup commits)
- Conventional Commits format: `fix(scope):` / `feat(scope):` / `refactor(scope):` / `docs(scope):`
- Commit only when explicitly requested by user — AGENTS.md §9
- Each commit message body explains WHY (not just WHAT) when non-obvious

## Success criteria
- **C1**: 4 probes pass (ERRNO_OK, STACK_OK, ALLOC_OK, DETACH_OK). No OOM/deadlock under multithread stress.
- **C2**: MPROTECT fault on PROT_NONE access + MPROTECT_RESTORE_OK. 3 docs fixed. No stale grep matches.
- **C3**: C3_GC_OTHERS_OK — MicroPython cross-thread GC deterministic pass, no UAF.
- **C4**: DRIVER_OK — framework crate compiles, dummy_driver registers. Dynamic IRQ trampoline: all baseline markers pass. DmaPool extracted, ext2 mounts.
- **C5**: XHCI_PCI_OK + XHCI_RESET_OK + XHCI_SLOT_OK + USB_KBD_OK. PS/2 regression passes.
- **C6**: ACPI_RSDP_OK + ACPI_TABLES_OK + ACPI_S5_OK. FADT-derived shutdown replaces hardcoded magic.
- **C7**: PIE_OK → DYN_OK 42 → DYN_TLS_OK. Static ET_EXEC regression passes.
- **Global**: ALL baseline harness cases still pass. Zero new syscalls. Zero new InvokeOps. No `as any`/`unwrap` in new code.
