# memory-and-doc-migration - Work Plan

## TL;DR (For humans)

**What you'll get:** CLUU gets every remaining memory-model upgrade (guard-page flags, a fast bump-pointer nursery, ASLR, /proc/meminfo, COW fork primitives, demand-paged text, and more — 16 items total) AND the scattered `docs/` directory is fully retired, with all its knowledge distilled into the single rustdoc-rendered `doc/` book (expanded from 13 to ~22 chapters). After this plan, there is one documentation tree, not two.

**Why this approach:** Two parallel tracks because memory code and doc migration have zero dependencies on each other — they can execute simultaneously. The doc migration extracts *knowledge* (design decisions, constraints, architecture) into book chapters rather than copying files, because the user explicitly asked for knowledge transfer, not file migration. Original docs/ survives in git history; no archive directory.

**What it will NOT do:** It will not re-architect the kernel memory subsystem — the linked-list allocator stays, the buddy PMM stays. It will not create a new documentation toolchain — the existing rustdoc-rendered `doc/src/lib.rs` pattern gets new modules appended. It will not touch the superpowers/ process or tooling.

**Effort:** XL — 26 todos across 10 waves, two parallel tracks. Metis-reviewed: 8 critical findings folded in. Dual Momus + Claude Code reviewed: 3 critical + 5 medium + 6 low issues folded in (wave conflicts fixed, COW fork QA deepened, superpowers manifest verification added, todos split, acceptance criteria made binary).
**Risk:** Medium — COW fork (M8) and frame typing (M16) are the highest-risk items; doc migration risks knowledge loss if extraction is sloppy; ASLR (M6) risks breaking the fault handler if hardcoded address consumers aren't fixed first.
**Decisions to sanity-check:** COW fork scope (COW primitive only, no full fork() semantics); frame typing scope (extend existing frame_table, don't duplicate); superpowers/ fate (extract + git-history only, no archive); M15 scope (MAP_SHARED wrapper only, no shm_open).

Your next move: approve to start execution, or run a high-accuracy review first. Full execution detail follows below.

---

> TL;DR (machine): XL effort, Medium risk, 26 todos — 14 memory code upgrades + full docs/→doc/book/ knowledge migration with docs/ retirement. Metis-reviewed (8 critical findings folded), dual Momus+Claude-Code reviewed (3 critical + 5 medium + 6 low issues folded).

## Scope
### Must have
- All 16 remaining memory-model code upgrades (C3, C6, M1-M4, M6-M13, M15-M16)
- Full knowledge extraction from docs/ (11 top-level files + 66 superpowers/ files) into doc/book/
- doc/book/ expanded from 13 to ~22 chapters with new modules in doc/src/lib.rs
- Cross-reference updates (README.md, AGENTS.md, .rs comments, etc/envelopes.toml)
- docs/ directory deleted after knowledge transfer verified

### Must NOT have (guardrails, anti-slop, scope boundaries)
- No replacement of the linked-list allocator or buddy PMM — augment, don't replace
- No new documentation toolchain — use existing rustdoc include_str! pattern
- No archive directory for superpowers/ — git history is the archive
- No file-level copies from docs/ to doc/ — extract and rewrite knowledge, don't copy text
- No changes to the kernel's capability/IPC model — memory upgrades only
- No new syscalls — use existing InvokeOp dispatch path (except M7 MemoryPressure which adds an InvokeOp variant on the existing invoke path, not a new syscall)
- No POSIX shm_open/shm_unlink or /dev/shm filesystem (M15 reclassified — wrapper only)
- No commits without explicit user request
- No deletion of docs/ until ALL 26+ cross-references verified updated (todo 25 is HARD-GATED on todo 24)

## Verification strategy
> Zero human intervention - all verification is agent-executed.
- Test decision: tests-after for code upgrades; diff-based verification for doc migration
- Code: `cargo xtask build` (full kernel+userspace build) + `cargo test --manifest-path userspace/libcluu/Cargo.toml --features host-test` (81 tests)
- Docs: grep verification that no docs/ paths remain in any tracked file; verify doc/book/ chapter count; verify doc/src/lib.rs compiles via `cargo doc --manifest-path doc/Cargo.toml`
- Evidence: .omo/evidence/task-<N>-memory-and-doc-migration.<ext>

## Execution strategy
### Parallel execution waves
> Two tracks run in parallel. Track A (memory code) and Track B (doc migration) have zero dependencies between them.

### Dependency matrix
| Todo | Depends on | Blocks | Can parallelize with | Wave |
| --- | --- | --- | --- | --- |
| 1 (C3 MAP_GUARD) | none | 5, 6, 11 | 2, 3, 4, 14-22 | 1 |
| 2 (M1+M11 proc/meminfo) | none | none | 1, 3, 4, 14-22 | 1 |
| 3 (M2 fragmentation) | none | none | 1, 2, 4, 14-22 | 1 |
| 4 (M3+M4 canaries+TLS) | none | none | 1, 2, 3, 14-22 | 1 |
| 5 (C6 nursery) | 1 | none | 6, 7, 14-22 | 2 |
| 6 (M6 ASLR) | 1 | 8 | 5, 7, 14-22 | 2 |
| 7 (M9 demand-paged text) | C5 (done) | none | 5, 6, 14-22 | 2 |
| 8 (M7 pressure API) | 6 | none | 9, 14-22 | 3 |
| 9 (M16 frame typing) | none | 10 | 8, 14-22 | 3 |
| 10 (M8 COW fork) | 9 | none | 11, 12, 13, 14-22 | 4 |
| 11 (M10 stack growth) | 1 | none | 10, 12, 13, 14-22 | 4 |
| 12 (M12 mmap expansion) | none | none | 10, 11, 13, 14-22 | 4 |
| 12b (M13 mremap) | none | none | 10, 11, 12, 14-22 | 4 |
| 13 (M15 shm wrapper) | none | none | 10, 11, 12, 12b, 14-22 | 4 |
| 14 (ARCH→architecture.md) | none | 23 | 1-13, 15-22 | 5 |
| 15 (INTERNALS→kernel+memory+ipc) | none | 23 | 1-13, 14, 16-22 | 5 |
| 16 (PID→sessions+procmgr+process_model) | none | 23 | 1-13, 14-15, 17-22 | 5 |
| 17 (ROADMAP+AUDIT→roadmap+audit) | none | 23 | 1-13, 14-16, 18-22 | 6 |
| 18 (debug+HARNESS→debugging+testing) | none | 23 | 1-13, 14-17, 19-22 | 6 |
| 19a (IPC_REG→capability_tokens) | none | 23 | 1-13, 14-18, 19b-22 | 6 |
| 19b (REPO_LAYOUT→getting_started) | none | 23 | 1-13, 14-18, 19a, 19c-22 | 6 |
| 19c (PORTING→interpreter_porting) | none | 23 | 1-13, 14-18, 19a-b, 19d-22 | 6 |
| 19d (FINDINGS distribution) | none | 23 | 1-13, 14-18, 19a-c, 19e, 20-22 | 6 |
| 19e (gotchas move+expand) | none | 23 | 1-13, 14-18, 19a-d, 20-22 | 6 |
| 20 (superpowers/specs extraction) | none | 23 | 1-13, 14-19, 21-22 | 7 |
| 21 (superpowers/plans extraction) | none | 23 | 1-13, 14-20, 22 | 7 |
| 22 (assets move) | none | 23 | 1-13, 14-21 | 7 |
| 23 (book restructure + lib.rs) | 14-22 | 24 | 1-13 | 8 |
| 24 (cross-ref updates) | 23 | 25 | 1-13 | 9 |
| 25 (docs/ retirement) | 24 | none | 1-13 | 10 |

> Wave conflicts fixed (Claude Code issue #1): todo 5 no longer blocks todo 6 (ASLR randomizes spawn-time base, nursery initializes after spawn — independent). Todo 10 (COW fork) moved to Wave 4, after todo 9 (frame typing) in Wave 3. Todo 12 split into 12 (M12 mmap expansion) + 12b (M13 mremap). Todo 19 split into 19a-19e.

## Todos
> Implementation + Test = ONE todo. Never separate.
<!-- APPEND TASK BATCHES BELOW THIS LINE WITH edit/apply_patch - never rewrite the headers above. -->

- [x] 1. C3: MAP_GUARD flag for space_map_range
  What to do: Add a `MAP_GUARD` flag bit (0x2000) to the kernel's `invoke_space_map_range` handler. SEMANTIC: when MAP_GUARD is set, the kernel installs a page-table entry with `present: false` (not-present) — no physical frame is allocated. Any access to a MAP_GUARD page triggers a page fault, which the kernel treats as an unrecoverable user fault (kills the thread) unless a fault_endpoint handler is registered (in which case the fault is forwarded). This is the primitive for explicit guard pages anywhere in the address space, and for write-barrier pages when combined with fault_endpoint. Add the constant `pub const MAP_GUARD: usize = 0x2000;` to `userspace/libcluu/src/syscall.rs` alongside existing MAP constants (MAP_DEVICE=0x100, MAP_FRAME_TOKEN=0x400, MAP_DEVICE_WC=0x1000, MAP_SHARE_PHYS=0x800). Update `map_stack` in `userspace/libcluu/src/process.rs` to optionally use MAP_GUARD for the guard page below the stack (currently the guard is implicit via STACK_STEP gap; this makes it explicit and enables guard pages anywhere). In the kernel handler at `handlers.rs:2045-2120`, when MAP_GUARD bit is set: skip frame allocation, install a PTE with present=false + user=true (so the fault is a user fault, not a kernel fault). Must NOT do: do not change existing MAP_DEVICE/MAP_DEVICE_WC/MAP_FRAME_TOKEN/MAP_SHARE_PHYS flag values. Do not allocate a physical frame for MAP_GUARD pages.
  Parallelization: Wave 1 | Blocked by: none | Blocks: 5
  References: kernel/src/syscall/handlers.rs:1422-1424 (MAP constants — pattern to follow), :2045-2120 (invoke_space_map_range handler — add MAP_GUARD branch), :2118 (flag parsing); userspace/libcluu/src/syscall.rs:970-992 (space_map_range — add MAP_GUARD constant); userspace/libcluu/src/process.rs:120-140 (map_stack — use MAP_GUARD); userspace/init/src/wiring.rs:121-125 (PROC_STACK_SIZE, STACK_STEP)
  Acceptance criteria: `cargo xtask build` succeeds. New test: create a MAP_GUARD region, attempt to read it, verify page fault occurs (kernel kills thread). Verify existing stack mapping still works (harness boot test). Verify MAP_GUARD page does NOT consume a physical frame (check pmm_get_stats before/after).
  QA scenarios: (happy) `cargo xtask build` passes + boot via `scripts/harness_run.sh` reaches login prompt; (failure) unit test that maps a MAP_GUARD page and reads it → thread killed, serial log shows "PF: killing thread". Evidence: .omo/evidence/task-1-memory-and-doc-migration.txt
  Commit: Y | feat(kernel): add MAP_GUARD flag for explicit guard-page mapping

- [x] 2. M1+M11: Verify /proc/meminfo + add per-process heap stats
  What to do: M1 is ALREADY IMPLEMENTED — `gen_meminfo()` at `userspace/vfs/src/procfs.rs:123` already calls `pmm_get_stats` and formats MemTotal/MemFree/MemAvailable/MemUsed. This todo is VERIFICATION + ENHANCEMENT: (1) verify `/proc/meminfo` returns real values via harness boot; (2) add per-process heap stats (`allocator::stats()` → AllocStats: total, used, peak, free) to `/proc/<pid>/status` via the procfs backend. The procfs backend lives in `userspace/vfs/src/procfs.rs`; `top` already reads `/proc/meminfo` (userspace/top/src/main.rs:495). `/proc/<pid>/status` currently only has Name/State/Pid/PPid (procfs.rs:299) — add HeapTotal/HeapUsed/HeapPeak/HeapFree fields. Must NOT do: do not add new syscalls — pmm_get_stats and space_get_stats already exist.
  Parallelization: Wave 1 | Blocked by: none | Blocks: none
  References: userspace/vfs/src/procfs.rs:123 (gen_meminfo — ALREADY IMPLEMENTED), :299 (/proc/<pid>/status — ADD heap fields); userspace/libcluu/src/syscall.rs:1434-1447 (pmm_get_stats), :1449-1459 (space_get_stats); userspace/libcluu/src/allocator.rs:46-52 (AllocStats), :514-519 (stats fn); userspace/top/src/main.rs:493-495 (reads /proc/meminfo); docs/PROCESS_ISOLATION_DESIGN.md:2777 (N8 — note: partially done)
  Acceptance criteria: `cargo xtask build` succeeds. Boot CLUU, run `cat /proc/meminfo` → shows MemTotal/MemFree with numeric values. `cat /proc/2/status` → shows HeapTotal/HeapUsed/HeapPeak/HeapFree fields.
  QA scenarios: (happy) boot + `cat /proc/meminfo` returns non-empty with matching MemTotal+MemFree=MemTotal; (failure) if pmm_get_stats returns 0 (PMM not ready), /proc/meminfo shows zeros, not a crash. Evidence: .omo/evidence/task-2-memory-and-doc-migration.txt
  Commit: Y | feat(procfs): add per-process heap stats to /proc/<pid>/status

- [x] 3. M2: Heap fragmentation reporting
  What to do: Add a `fragmentation()` method to `LinkedListAllocator` in `userspace/libcluu/src/allocator.rs` that returns the ratio of largest-free-block to total-free-bytes. Expose via `AllocStats` (add `largest_free: usize` field) and `allocator::stats()`. This helps debug "OOM with free memory available" scenarios. Must NOT do: do not change the allocator algorithm.
  Parallelization: Wave 1 | Blocked by: none | Blocks: none
  References: userspace/libcluu/src/allocator.rs:193-201 (LinkedListAllocator struct), :244-254 (stats method), :356-372 (coalesce — walks free list), :404-450 (try_alloc — walks free list); userspace/libcluu/src/allocator.rs:46-52 (AllocStats)
  Acceptance criteria: `cargo test --manifest-path userspace/libcluu/Cargo.toml --features host-test` passes. New unit test: allocate 3 blocks, free middle one, verify largest_free equals the middle block size. `cargo xtask build` succeeds.
  QA scenarios: (happy) unit test shows correct largest_free after alloc/free pattern; (failure) alloc with empty heap → largest_free = 0, not panic. Evidence: .omo/evidence/task-3-memory-and-doc-migration.txt
  Commit: Y | feat(allocator): add heap fragmentation reporting to AllocStats

- [x] 4. M3+M4: Stack canaries + TLS destructor verification
  What to do: (M3) Add a canary word (0xDEADBEEFCAFEBABE) at the bottom of each process stack (above the guard page) in `map_stack` or `launch_service`. Check the canary on thread exit in procmgr's exit handler. APPROACH DECIDED: extend `ThreadGetStats` invoke op (kernel/src/syscall/handlers.rs — search for invoke_thread_get_stats) to return stack_base + canary_offset, so procmgr can read the canary word on thread exit. Alternatively, procmgr already knows stack_top from spawn (wiring.rs:325 `PROC_STACK_TOP - index * STACK_STEP`) — compute stack_base from that and read the canary at stack_base + 8. Use the procmgr-known-address approach (no new invoke op needed). (M4) VERIFICATION ONLY — `run_key_destructors` at `userspace/libcluu/src/posix/pthread.rs:985` ALREADY EXISTS and runs 4 POSIX rounds on both `pthread_entry` (l.293) and `pthread_exit` (l.1042). Write a test probe that creates 4+ pthread keys with destructors, thread exits, verify all destructors ran (side-effect observable). If test passes, M4 is done — no code change needed. Must NOT do: do not change the canary value at runtime; do not add new syscalls for canary checking.
  Parallelization: Wave 1 | Blocked by: none | Blocks: none
  References: userspace/libcluu/src/process.rs:120-140 (map_stack); userspace/init/src/wiring.rs:325-331 (stack mapping in launch_service); userspace/libcluu/src/posix/pthread.rs:289-321 (pthread_entry — exit path), :985 (run_key_destructors); userspace/root-procmgr/src/main.rs (exit handling — search for thread exit/kill paths)
  Acceptance criteria: `cargo xtask build` succeeds. Boot CLUU, run several processes, exit them — no canary corruption serial output. `cargo test --manifest-path userspace/libcluu/Cargo.toml --features host-test` passes.
  QA scenarios: (happy) boot + run `micropython -c "print(1)"` + exit → no canary warning in serial; (failure) if canary is corrupted, serial shows "STACK CANARY CORRUPTED" and procmgr logs it. Evidence: .omo/evidence/task-4-memory-and-doc-migration.txt
  Commit: Y | feat(userspace): add stack canaries and verify TLS destructor cleanup

- [x] 5. C6: Bump-pointer nursery in libcluu
  What to do: Add a 1-2 MB bump-pointer nursery in front of `LockedAllocator` in `userspace/libcluu/src/allocator.rs`. Allocations <256 bytes go to the nursery (fast path: increment a pointer). Large allocations and all deallocs go to the linked-list allocator. When the nursery is full, sweep: free all nursery blocks (nursie is "allocation-only", no individual frees — it's a bump allocator that gets reset). Pattern: jemalloc tcache, tcmalloc FreeList. Must NOT do: do not replace LockedAllocator; do not change the global_allocator — the nursery wraps it.
  Parallelization: Wave 2 | Blocked by: 1 | Blocks: 6
  References: userspace/libcluu/src/allocator.rs:485-540 (LockedAllocator — wrap this), :134-140 (size constants), :404-465 (alloc path — add nursery check before try_alloc); userspace/libcluu/src/posix/memory.rs:73-79 (mmap region — could mmap the nursery)
  Acceptance criteria: `cargo xtask build` succeeds. `cargo test --manifest-path userspace/libcluu/Cargo.toml --features host-test` passes. New benchmark test: 1000 small allocs with nursery vs without — nursery path is faster (or at least not slower). Boot CLUU + run MicroPython → no OOM with nursery.
  QA scenarios: (happy) boot + `micropython -c "for i in range(1000): x = [1]*100"` succeeds; (failure) nursery full → sweep + fallback to linked-list, no panic. Evidence: .omo/evidence/task-5-memory-and-doc-migration.txt
  Commit: Y | feat(allocator): add bump-pointer nursery for small allocations

- [x] 6. M6: ASLR — address space layout randomization
  What to do: PREREQUISITE: fix global-layout consumers BEFORE randomizing. `kernel/src/architecture/x86_64/idt.rs:981-983` consumes global `layout::USER_STACK_BOTTOM`/`USER_STACK_TOP`/`USER_HEAP_START` constants for fault classification — ASLR requires these to become per-process values (stored in AddressSpace or thread context), not literal hardcode removal. NOTE: `kernel/src/mm/fault.rs:284` (`let heap_addr = VirtAddr::new(0x00800000)`) is in TEST CODE (MockMapper test), not production — fix it to use `layout::USER_HEAP_START` for consistency but it's not an ASLR blocker. Then: randomize the base addresses of stack, heap, and mmap regions per process. Currently: PROC_STACK_BASE=0x6f000000 (wiring.rs:122), USER_HEAP_START=0x0080_0000 (allocator.rs:146), MMAP_REGION_START=0x4100_0000 (posix/memory.rs:73). Add a random page-aligned offset from `klibcluu::crypto::random::random_u64()` (already used by OpaqueScope::random) to each base at process spawn time. The kernel's USER_STACK_SIZE=16MB region (space.rs:52) has headroom for randomization. NOTE: no general mmap allocator exists for mmap base randomization — mmap uses caller-supplied virt or first-fit in fixed region; mmap ASLR means randomizing MMAP_REGION_START, not individual allocations. Must NOT do: do not randomize kernel addresses; do not break existing ELF loading (code/data segments stay at fixed p_vaddr — only stack/heap/mmap base randomizes).
  Parallelization: Wave 2 | Blocked by: 1 | Blocks: 8
  References: userspace/init/src/wiring.rs:121-125 (PROC_STACK_BASE, STACK_STEP); userspace/libcluu/src/allocator.rs:146 (USER_HEAP_START); userspace/libcluu/src/posix/memory.rs:73 (MMAP_REGION_START); kernel/src/mm/space.rs:48-52 (USER_HEAP_START, USER_STACK_SIZE); klibcluu/src/crypto/random.rs:124 (random_u64); kernel/src/architecture/x86_64/idt.rs:981-983 (HARDCODED — MUST FIX); kernel/src/mm/fault.rs:284 (HARDCODED 0x00800000 — MUST FIX); kernel/src/token/mod.rs (OpaqueScope::random uses random_u64)
  Acceptance criteria: `cargo xtask build` succeeds. Boot CLUU twice — verify stack addresses differ across boots (via serial log or /proc/<pid>/status). All existing harness tests pass. Fault handler correctly classifies stack vs heap faults with randomized addresses.
  QA scenarios: (happy) two boots show different stack addresses, all services start normally; (failure) random offset causes overlap with ELF segments → boot fails with clear error, not silent corruption; fault handler misclassifies fault → verify idt.rs uses per-process layout, not hardcoded constants. Evidence: .omo/evidence/task-6-memory-and-doc-migration.txt
  Commit: Y | feat(kernel): add ASLR for stack, heap, and mmap regions

- [x] 7. M9: Demand-paged text segments
  What to do: PREREQUISITE: `invoke_space_protect` (handlers.rs:1631-1708) currently validates mapping presence first (line 1674-1679) and returns `Err(NotFound)` for unmapped pages. M9 needs to install not-present PTEs for unmapped text pages so they fault on first execution. Either (a) change `space_protect` to allow installing not-present PTEs for unmapped addresses (creates a PTE without a backing frame), or (b) map-then-protect (wasteful: allocates a frame then immediately makes it not-present). Option (a) is correct — add a new `SpaceProtectUnmapped` invoke op OR extend `SpaceProtect` to accept a "create entry if unmapped" flag. Then: map ELF `.text` segments as not-present in `map_segment` (userspace/libcluu/src/process.rs:61-118). When the kernel's page-fault handler fires on a text address, demand-page it with read+exec permissions. The kernel's `handle_heap_fault` (idt.rs:972-1039) already demand-pages heap; add a parallel `handle_text_fault` that maps with read+exec (not read+write). Must NOT do: do not change .data/.bss mapping — those stay eagerly mapped. Do not break the existing boot path.
  Parallelization: Wave 2 | Blocked by: C5 (done — PROT_NONE kernel support landed) | Blocks: none
  References: kernel/src/syscall/handlers.rs:1631-1708 (invoke_space_protect — MUST EXTEND to allow unmapped pages); kernel/src/architecture/x86_64/idt.rs:972-1039 (handle_heap_fault — add parallel handle_text_fault); userspace/libcluu/src/process.rs:61-118 (map_segment — page_flags()); kernel/src/mm/fault.rs:96-126 (FaultHandler::handle — fault routing); kernel/src/mm/space.rs:48-49 (USER_HEAP_START/MAX — text is below heap)
  Acceptance criteria: `cargo xtask build` succeeds. Boot CLUU — all services start. Serial log shows demand-fault for text pages (add a trace). Memory usage at boot is lower (measure via /proc/meminfo before/after).
  QA scenarios: (happy) boot succeeds, services run, text pages fault in on first execution; (failure) if a service's entry point is in an unmapped page and the fault handler can't resolve it → clear error, not hang. Evidence: .omo/evidence/task-7-memory-and-doc-migration.txt
  Commit: Y | feat(kernel): demand-page text segments to reduce boot memory

- [x] 8. M7: Memory pressure API
  What to do: Add a `MemoryPressure` invoke op (new InvokeOp variant). DUAL ENUM UPDATE REQUIRED: add to BOTH `kernel/src/token/mod.rs:416` (InvokeOp enum + `from_usize` match arm) AND `userspace/libcluu/src/syscall.rs:145` (InvokeOp enum + `from_usize` match arm). procmgr can call it to ask processes to release caches. Combined with the existing OOM callback (C4, done), this gives a two-tier response: pressure → release caches, OOM → run GC. The invoke op sends a notification to a registered pressure endpoint on the target process. Must NOT do: do not add per-page pressure tracking — this is a coarse-grained API. Do not forget the `from_usize` match arm in BOTH enums — mismatched enums cause silent dispatch failures.
  Parallelization: Wave 3 | Blocked by: 6 | Blocks: none
  References: kernel/src/token/mod.rs:416 (InvokeOp enum — ADD variant + from_usize arm); userspace/libcluu/src/syscall.rs:145 (InvokeOp enum — ADD variant + from_usize arm — MUST MATCH KERNEL); kernel/src/syscall/handlers.rs (invoke dispatch — add handler); userspace/libcluu/src/allocator.rs (OOM handler — C4 done); userspace/root-procmgr/src/main.rs (procmgr — caller)
  Acceptance criteria: `cargo xtask build` succeeds. New unit test: register a pressure handler, send pressure notification, verify handler is called. Boot CLUU — no regressions. Verify InvokeOp variant number matches in both kernel and libcluu enums.
  QA scenarios: (happy) pressure notification received and handler called; (failure) no handler registered → notification is a no-op, no crash. Evidence: .omo/evidence/task-8-memory-and-doc-migration.txt
  Commit: Y | feat(kernel): add MemoryPressure invoke op for cache release signaling

- [x] 9. M16: Frame typing — verify existing + enforce refcount (Phase 1 → Phase 2)
  What to do: VERIFICATION + ENFORCEMENT, not new construction. The frame typing system ALREADY EXISTS: `kernel/src/mm/frame_table.rs` has `FrameTag` enum with all 7 variants (Untyped=0, UserData=1, PageTable=2, Grant=3, Device=4, KernelHeap=5, BootReserved=6 — lines 81-89), `FrameMeta` struct (tag, refcount, owner, extra — lines 94-105), `inc_ref`/`dec_ref` with auto UserData→Grant transition (lines 358-390), and `SpaceDestroy` already calls `dec_ref` (handlers.rs:1405, 1612). The GAP: refcount is "Phase 1: advisory, not enforced" (frame_table.rs:99, :278, :332, :604). M16 tasks: (1) audit all `inc_ref`/`dec_ref` call sites (handlers.rs:1292, 1405, 1483-1485, 1546, 1556, 1612, 2013, 2243, 2259) and verify they're correctly paired; (2) flip from advisory to enforced — when `dec_ref` drops refcount to 0, the frame MUST be returned to PMM (currently advisory); (3) verify `retag_pt_owner` (handlers.rs:1292) correctly tags page-table frames on space creation; (4) add a leak-detection test: create space, map pages, destroy space, verify all frames returned to PMM (pmm_get_stats before/after). Must NOT do: do not create a second frame-typing system — the existing `frame_table.rs` IS the system. Do not replace the buddy allocator.
  Parallelization: Wave 3 | Blocked by: none | Blocks: 10
  References: kernel/src/mm/frame_table.rs:81-89 (FrameTag enum — ALREADY EXISTS), :94-105 (FrameMeta — ALREADY EXISTS), :99 (refcount "advisory, not enforced" — THE GAP), :278 (retype "advisory"), :332 (unretype "advisory"), :358-390 (inc_ref/dec_ref with auto-transition), :604 (inc_ref "Phase 1-era"); kernel/src/syscall/handlers.rs:1292 (retag_pt_owner), :1404-1405 (SpaceDestroy dec_ref), :1483-1485 (inc_ref on grant), :1546 (dec_ref), :1556 (dec_ref), :1606-1612 (SpaceDestroy dec_ref), :2013 (inc_ref), :2243 (inc_ref), :2259 (dec_ref); kernel/src/mm/pmm.rs (buddy allocator — unchanged); docs/superpowers/specs/2026-05-18-frame-typing-and-unified-process-model.md (spec — for reference)
  Acceptance criteria: `cargo xtask build` succeeds. New kernel test: create space, map N pages, destroy space, verify pmm_get_stats shows N frames returned (used count drops by N). Boot CLUU — no regressions. Grep verifies no "advisory" or "Phase 1" comments remain in frame_table.rs (all flipped to enforced).
  QA scenarios: (happy) frame lifecycle: alloc→retype→map→unmap→dec_ref→free works, frames return to PMM; (failure) double-dec_ref → detected and logged, not silent underflow; (leak) create+destroy 100 spaces → pmm_get_stats used count returns to baseline. Evidence: .omo/evidence/task-9-memory-and-doc-migration.txt
  Commit: Y | feat(kernel): enforce frame refcount (Phase 1 advisory → Phase 2 enforced)

- [x] 10. M8: COW fork primitive
  What to do: Implement a copy-on-write fork primitive. On fork: create a new address space (space_create), share all physical frames from parent as read-only (via space_protect with present=false for write-barrier — C5 done), record parent→child frame mappings. On write fault: allocate a new frame, copy parent's page, map it writable in the faulting space. The fault_endpoint mechanism (ThreadSetFaultEndpoint) handles the write-fault — the fork helper registers as fault handler on the child. API: compose existing invoke ops (space_create + space_protect + ThreadSetFaultEndpoint — no new syscall). The fork trigger is a userspace function in libcluu that calls these ops in sequence. This is the largest single item. Must NOT do: do not implement full POSIX fork() semantics (signal handlers, fd table copy, etc.) — just the memory COW primitive. Do not add new syscalls.
  Parallelization: Wave 4 | Blocked by: 9 | Blocks: none
  References: kernel/src/syscall/handlers.rs:1631-1708 (invoke_space_protect — C5 fix makes present: false possible); kernel/src/architecture/x86_64/idt.rs:425-520 (try_forward_fault — fault handling for COW); kernel/src/mm/space.rs (AddressSpace — new space creation); kernel/src/mm/space_repository.rs (space lookup); userspace/libcluu/src/syscall.rs:1028-1044 (space_protect); docs/PROCESS_ISOLATION_DESIGN.md (process model — fork semantics); kernel/src/syscall/handlers.rs:1025-1062 (invoke_thread_set_fault_endpoint — fault handler registration)
  Acceptance criteria: `cargo xtask build` succeeds. New kernel test: create space A, map a page, fork to space B, write to the page in B, verify B gets a private copy (different physical frame) and A's page is unchanged. Boot CLUU — no regressions.
  QA scenarios: (happy) COW fork: child writes → gets private page, parent's page unchanged; (failure) parent has unmapped page → child fault on that page → kills child, not parent; (nested) fork child forks grandchild — grandchild writes, both parent and grandparent pages unchanged; (concurrent) two child threads fault on same COW page simultaneously — both get private copies, no refcount race; (refcount-zero) fork N children, all exit → parent's frames refcount returns to 1 (exclusive), no leak. Evidence: .omo/evidence/task-10-memory-and-doc-migration.txt
  Commit: Y | feat(kernel): implement COW fork primitive with write-fault handling

- [x] 11. M10: Stack growth
  What to do: CLARIFICATION: the kernel's `handle_heap_fault` (idt.rs:972-1039) ALREADY demand-pages the entire stack REGION (`USER_STACK_BOTTOM..USER_STACK_TOP`, 16 MB — see idt.rs:981-982). The 64 KB `PROC_STACK_SIZE` (wiring.rs:121) is just the initially-mapped portion. What's MISSING: the kernel demand-pages any fault in the 16 MB stack region, but it maps with read+write (heap flags) — it should map stack pages with read+write+no-exec. Also, there's no explicit "stack growth" notification — the kernel silently maps pages on fault. M10 should: (1) verify stack-region faults already work (they should — handle_heap_fault covers them); (2) add a stack-specific fault path that uses read+write+no-exec flags instead of heap flags; (3) add a stack-limit check so a runaway stack doesn't consume all 16 MB silently — log a warning at 1 MB, 4 MB, 8 MB thresholds. Must NOT do: do not grow stacks beyond USER_STACK_SIZE (16 MB); do not grow stacks for threads that don't opt in.
  Parallelization: Wave 4 | Blocked by: 1 | Blocks: none
  References: kernel/src/architecture/x86_64/idt.rs:972-1039 (handle_heap_fault — extend or parallel); kernel/src/mm/space.rs:52 (USER_STACK_SIZE=16MB); userspace/init/src/wiring.rs:121 (PROC_STACK_SIZE=64KB — the initially-mapped portion); kernel/src/mm/fault.rs:96-126 (FaultHandler::handle)
  Acceptance criteria: `cargo xtask build` succeeds. New test: process with 64KB stack calls a recursive function 1000 levels deep — succeeds instead of crashing. Boot CLUU — no regressions.
  QA scenarios: (happy) deep recursion succeeds, stack grew to accommodate; (failure) recursion exceeds 16MB → thread killed with stack overflow, not silent corruption. Evidence: .omo/evidence/task-11-memory-and-doc-migration.txt
  Commit: Y | feat(kernel): add stack growth via demand paging

- [x] 12. M12: mmap region expansion
  What to do: Replace the fixed 64-entry `MmapRegionTable` in `userspace/libcluu/src/posix/memory.rs:92-94` (MAX_MMAP_REGIONS=64 at line 79) with a resizable structure (Vec<MmapRegion> or a BTreeMap keyed by addr) so the 64-region limit doesn't block server workloads. Must NOT do: do not change the mmap region address range (0x4100_0000..0x5000_0000). Do not break code that iterates entries (find_exact, update_prot_exact, overlaps, find_first_fit — all in memory.rs:124-163).
  Parallelization: Wave 4 | Blocked by: none | Blocks: none
  References: userspace/libcluu/src/posix/memory.rs:79 (MAX_MMAP_REGIONS=64), :92-164 (MmapRegionTable — replace), :103-111 (insert), :113-122 (remove), :124-131 (find_exact), :133-141 (update_prot_exact), :143-151 (overlaps), :153-163 (find_first_fit)
  Acceptance criteria: `cargo xtask build` succeeds. New test: mmap 100 regions — all succeed (was: 64 max). Boot CLUU — no regressions.
  QA scenarios: (happy) 100 mmap regions succeed; (failure) mmap with no memory left → ENOMEM, not panic. Evidence: .omo/evidence/task-12-memory-and-doc-migration.txt
  Commit: Y | feat(userspace): expand mmap region limit from fixed 64 to resizable

- [x] 13. M13: mremap
  What to do: Add an `mremap` function to `userspace/libcluu/src/posix/memory.rs` that resizes an existing mapping in place — useful for growing buffer pools without copying. DUAL ENUM UPDATE: if mremap needs a new InvokeOp variant (kernel/src/token/mod.rs:416 + userspace/libcluu/src/syscall.rs:145 — both must match, including from_usize arms, same pattern as M7 todo 8). If it can be composed from existing space_unmap + space_map_range (unmap old, map new at same or different addr), do that instead — no new InvokeOp needed. Prefer the compose approach to avoid dual-enum maintenance burden. Must NOT do: do not change the mmap region address range.
  Parallelization: Wave 4 | Blocked by: none | Blocks: none
  References: userspace/libcluu/src/posix/memory.rs:455-499 (munmap — pattern to follow), :506-558 (mprotect — pattern to follow); userspace/libcluu/src/syscall.rs:1000-1012 (space_unmap), :970-992 (space_map_range); kernel/src/token/mod.rs:416 (InvokeOp — add variant ONLY if needed); kernel/src/syscall/handlers.rs (add handler ONLY if needed)
  Acceptance criteria: `cargo xtask build` succeeds. New test: mmap a region, mremap to 2x size — succeeds, same or new address. Boot CLUU — no regressions.
  QA scenarios: (happy) mremap grows region in place; (failure) mremap with invalid addr → EINVAL, not panic. Evidence: .omo/evidence/task-12b-memory-and-doc-migration.txt
  Commit: Y | feat(userspace): add mremap for in-place mapping resize

- [x] 14. M15: Shared memory IPC (RECLASSIFIED — feature, not memory model)
  What to do: NOTE: Metis flagged this as scope creep — `shm_open`/`MAP_SHARED` between processes is a full IPC mechanism, not a memory model improvement. It violates §2 (no new syscalls for new userspace features — would need new `InvokeOp` or shared endpoint semantics). RECLASSIFY: expose `MAP_SHARE_PHYS` (already in kernel — handlers.rs:2076) as a userspace API, but do NOT implement `shm_open`/`shm_unlink` or a `/dev/shm` filesystem. Just add a `MAP_SHARED` path to the existing `mmap` that routes to `MAP_SHARE_PHYS` so two processes can share a physical frame by agreement (e.g., compositor SHM cells already do this via `MAP_SHARE_PHYS` at terminal.md:126). This is documentation + wrapper, not new IPC. Must NOT do: do not implement POSIX `shm_open`/`shm_unlink` — that's a separate feature track. Do not create `/dev/shm`.
  Parallelization: Wave 4 | Blocked by: none | Blocks: none
  References: kernel/src/syscall/handlers.rs:2076 (MAP_SHARE_PHYS=0x800 — already exists); userspace/libcluu/src/posix/memory.rs (mmap — add MAP_SHARED path); userspace/libcluu/src/syscall.rs (space_map_range); doc/book/terminal.md:126 (existing MAP_SHARE_PHYS usage — compositor SHM); docs/ROADMAP.md:166 (MAP_SHARE_PHYS UAF history — read for context on known bug)
  Acceptance criteria: `cargo xtask build` succeeds. Existing compositor SHM still works. Document the MAP_SHARED wrapper in doc/book/ipc.md or doc/book/memory_model.md.
  QA scenarios: (happy) existing SHM-using services still boot; (failure) unmap shared page → no UAF (the ROADMAP:166 bug must not recur). Evidence: .omo/evidence/task-13-memory-and-doc-migration.txt
  Commit: Y | feat(userspace): expose MAP_SHARE_PHYS as MAP_SHARED wrapper in mmap

- [x] 15. Doc migration: ARCHITECTURE.md → architecture.md
  What to do: Read `docs/ARCHITECTURE.md` (414 lines) and `doc/book/architecture.md` (202 lines). Extract knowledge from the docs/ version that is MISSING from the book version — deeper subsystem descriptions, mermaid diagrams, IPC flow details, spawn flow. Merge into `doc/book/architecture.md` without duplicating existing book content. Must NOT do: do not copy text verbatim — rewrite for the book's style (terse, technical). Do not delete docs/ARCHITECTURE.md yet (that's todo 25).
  Parallelization: Wave 5 (parallel with track A) | Blocked by: none | Blocks: 23
  References: docs/ARCHITECTURE.md (source); doc/book/architecture.md (target — read existing content first); doc/src/lib.rs:50-51 (include_str pattern)
  Acceptance criteria: `doc/book/architecture.md` contains all knowledge from `docs/ARCHITECTURE.md`. `cargo doc --manifest-path doc/Cargo.toml` succeeds. No content is duplicated — the book version subsumes the docs/ version.
  QA scenarios: (happy) diff check: every major section in docs/ARCHITECTURE.md has a corresponding section in doc/book/architecture.md; (failure) `cargo doc` fails → fix compile error. Evidence: .omo/evidence/task-14-memory-and-doc-migration.txt
  Commit: Y | docs(book): merge ARCHITECTURE.md knowledge into architecture chapter

- [x] 16. Doc migration: INTERNALS.md → kernel.md + memory_model.md + ipc.md
  What to do: Read `docs/INTERNALS.md` (654 lines). Split into three targets: (1) `doc/book/kernel.md` — subsystem deep-dives that aren't already in the book version (215 lines); (2) NEW `doc/book/memory_model.md` — the memory model section (address space layout, heap/stack/mmap, allocator, demand paging, fault handling, guard pages); (3) NEW `doc/book/ipc.md` — the IPC section (message format, endpoint model, call/reply, invoke ops). Must NOT do: do not duplicate content already in kernel.md — only add what's missing.
  Parallelization: Wave 5 | Blocked by: none | Blocks: 23
  References: docs/INTERNALS.md (source — 654 lines); doc/book/kernel.md (target 1 — 215 lines existing); doc/book/memory_model.md (target 2 — new file); doc/book/ipc.md (target 3 — new file); doc/src/lib.rs:53-54 (kernel module), (add new modules for memory_model + ipc)
  Acceptance criteria: Three files exist and compile via `cargo doc --manifest-path doc/Cargo.toml`. Every section in INTERNALS.md has a home in one of the three files. No duplication.
  QA scenarios: (happy) `cargo doc` succeeds, all INTERNALS.md sections accounted for; (failure) missing section → grep verifies no INTERNALS.md heading is unaccounted. Evidence: .omo/evidence/task-15-memory-and-doc-migration.txt
  Commit: Y | docs(book): split INTERNALS.md into kernel, memory_model, and ipc chapters

- [x] 17. Doc migration: PROCESS_ISOLATION_DESIGN.md → sessions.md + procmgr.md + process_model.md
  What to do: Read `docs/PROCESS_ISOLATION_DESIGN.md` (2856 lines — the largest doc). Extract knowledge into three targets: (1) `doc/book/sessions.md` — session encapsulation, root godmode, session lifecycle; (2) `doc/book/procmgr.md` — procmgr design, spawn protocol, exit handling; (3) NEW `doc/book/process_model.md` — process isolation model, capability scoping, VFS view derivation, /proc design. This is the biggest extraction — focus on design decisions and constraints, not implementation details that are already in code. Must NOT do: do not copy 2856 lines — distill to ~300-400 lines per target chapter.
  Parallelization: Wave 5 | Blocked by: none | Blocks: 23
  References: docs/PROCESS_ISOLATION_DESIGN.md (source — 2856 lines); doc/book/sessions.md (target 1 — 231 lines existing); doc/book/procmgr.md (target 2 — 143 lines existing); doc/book/process_model.md (target 3 — new file)
  Acceptance criteria: Three files compile via `cargo doc`. EXTRACTION VERIFICATION: before distillation, extract a list of all `##` and `###` headings from PROCESS_ISOLATION_DESIGN.md into the evidence file. After distillation, grep each heading topic in the three target files — every heading must have a corresponding section (or be explicitly marked "deferred — see git history" in the evidence file). Total extracted content ~900-1200 lines (distilled from 2856).
  QA scenarios: (happy) `cargo doc` succeeds, design decisions preserved, heading-coverage grep passes; (failure) over-copying → file > 400 lines, needs further distillation; heading missing → grep finds gap, add it. Evidence: .omo/evidence/task-16-memory-and-doc-migration.txt
  Commit: Y | docs(book): distill PROCESS_ISOLATION_DESIGN into sessions, procmgr, and process_model

- [x] 18. Doc migration: ROADMAP.md → roadmap.md + KERNEL_AUDIT.md → audit.md
  What to do: Read `docs/ROADMAP.md` (268 lines) and `docs/KERNEL_AUDIT.md` (290 lines). Create NEW `doc/book/roadmap.md` — phase structure, exit criteria, known unknowns. Create NEW `doc/book/audit.md` — kernel audit findings, security posture, code quality notes. Must NOT do: do not include completed-and-forgotten items as "todo" — mark them done.
  Parallelization: Wave 6 | Blocked by: none | Blocks: 23
  References: docs/ROADMAP.md (268 lines); docs/KERNEL_AUDIT.md (290 lines); doc/src/lib.rs (add two new modules)
  Acceptance criteria: Both new files compile via `cargo doc`. ROADMAP content reflects current status (Phase 3 in progress). AUDIT content preserves finding IDs (C-8, etc.).
  QA scenarios: (happy) `cargo doc` succeeds; (failure) stale roadmap items → verify against code state. Evidence: .omo/evidence/task-17-memory-and-doc-migration.txt
  Commit: Y | docs(book): add roadmap and audit chapters from docs/

- [x] 19. Doc migration: debug-guide.md → debugging.md + HARNESS.md → testing.md
  What to do: Read `docs/debug-guide.md` (295 lines) and `docs/HARNESS.md` (38 lines). Create NEW `doc/book/debugging.md` — GDB setup, QEMU debug mode, serial log reading, common debug scenarios. Create NEW `doc/book/testing.md` — harness usage, test commands, marker mode, keystroke sequences. Must NOT do: do not include debug commands that no longer work.
  Parallelization: Wave 6 | Blocked by: none | Blocks: 23
  References: docs/debug-guide.md (295 lines); docs/HARNESS.md (38 lines); scripts/harness_run.sh (verify commands still work); doc/src/lib.rs (add two new modules)
  Acceptance criteria: Both new files compile via `cargo doc`. Debug commands verified current (QEMU_GDB, HARNESS_GDB_MODE, cargo xtask run --debug).
  QA scenarios: (happy) `cargo doc` succeeds; (failure) stale command → verify against scripts/harness_run.sh. Evidence: .omo/evidence/task-18-memory-and-doc-migration.txt
  Commit: Y | docs(book): add debugging and testing chapters from docs/

- [x] 20. Doc migration: IPC_REGISTRY + REPO_LAYOUT + INTERPRETER_PORTING + DOC-FINDINGS + gotchas (5 independent sub-tasks)
  What to do: Five INDEPENDENT doc merges — each has its own pass/fail gate so partial failure in one doesn't block the others:
  (19a) Merge `docs/IPC_REGISTRY.md` (71 lines) into `doc/book/capability_tokens.md` (127 lines) — IPC label table.
  (19b) Merge `docs/REPO_LAYOUT.md` (38 lines) into `doc/book/getting_started.md` (168 lines) — repo structure.
  (19c) Move `docs/INTERPRETER-PORTING.md` (138 lines, new from our earlier work) to `doc/book/interpreter_porting.md`.
  (19d) Distribute `docs/DOC-FINDINGS.md` (170 lines) findings into relevant chapters — each finding (F-001..F-008) goes into the chapter it affects. Record which chapter each F-NNN went into in the evidence file.
  (19e) Move `docs/gotchas/cluu-allocator-reentrancy-leak.md` to `doc/book/gotchas.md` (expand to cover all gotchas, not just one). Also check AGENTS.md §7 for a broken ref to `gotchas/cluu-single-threaded-mutual-blocking-ipc-deadlock` — if that gotcha file doesn't exist, either create a stub in gotchas.md or remove the ref from AGENTS.md (todo 24 handles AGENTS.md updates).
  Must NOT do: do not create a separate chapter for DOC-FINDINGS — distribute the findings.
  Parallelization: Wave 6 | Blocked by: none | Blocks: 23
  References: docs/IPC_REGISTRY.md; docs/REPO_LAYOUT.md; docs/INTERPRETER-PORTING.md; docs/DOC-FINDINGS.md; docs/gotchas/cluu-allocator-reentrancy-leak.md; doc/book/capability_tokens.md; doc/book/getting_started.md; doc/src/lib.rs; AGENTS.md §7 (broken gotcha ref)
  Acceptance criteria: All target files compile via `cargo doc`. No docs/ knowledge lost — grep verifies every finding ID (F-001..F-008) appears in some book chapter (evidence file records the mapping). Each of the 5 sub-tasks independently pass/fail.
  QA scenarios: (happy) `cargo doc` succeeds, all findings distributed, evidence file has F-NNN→chapter mapping; (failure) finding missing → grep finds it in a book chapter or evidence file explains deferral. Evidence: .omo/evidence/task-19-memory-and-doc-migration.txt
  Commit: Y | docs(book): distribute IPC registry, repo layout, interpreter porting, findings, gotchas

- [x] 21. Doc migration: superpowers/specs/ knowledge extraction
  What to do: Read all 20 files in `docs/superpowers/specs/` (design specs). Extract the DESIGN KNOWLEDGE from each — architecture decisions, constraints, trade-offs, gotchas — into the relevant book chapter. For example: frame-typing spec → process_model.md; session-lifecycle spec → sessions.md; spawn-unification spec → procmgr.md; terminal-pty spec → terminal.md; pipe spec → new section in services.md or ipc.md. Do NOT copy the specs — extract 3-10 lines of key decisions per spec and add to the relevant chapter. Must NOT do: do not create a "specs" chapter — the knowledge goes INTO existing chapters.
  Parallelization: Wave 7 | Blocked by: none | Blocks: 23
  References: docs/superpowers/specs/ (20 files); doc/book/*.md (all existing chapters — add to relevant ones)
  Acceptance criteria: Each spec file has its key decisions reflected in at least one book chapter. `cargo doc` succeeds. No spec is copied verbatim — all content is distilled.
  QA scenarios: (happy) `cargo doc` succeeds, spec knowledge distributed; (failure) spec missing → for each spec, grep its title in doc/book/ to verify extraction. Evidence: .omo/evidence/task-20-memory-and-doc-migration.txt
  Commit: Y | docs(book): extract design knowledge from superpowers specs into book chapters

- [x] 22. Doc migration: superpowers/plans/ knowledge extraction
  What to do: Read all 40+ files in `docs/superpowers/plans/` (implementation plans). Extract IMPLEMENTATION LESSONS — what worked, what didn't, gotchas discovered, testing patterns — into the relevant book chapter or the gotchas chapter. These plans are mostly executed; their value now is lessons learned, not step-by-step instructions. Distill each plan to 2-5 lines of key insight. Must NOT do: do not copy implementation steps — they're already reflected in code.
  Parallelization: Wave 7 | Blocked by: none | Blocks: 23
  References: docs/superpowers/plans/ (40+ files); doc/book/*.md (all existing chapters); doc/book/gotchas.md (many lessons become gotchas)
  Acceptance criteria: Each plan file has at least one lesson or gotcha extracted into a book chapter. `cargo doc` succeeds.
  QA scenarios: (happy) `cargo doc` succeeds; (failure) plan with no extractable lesson → skip with note in evidence file. Evidence: .omo/evidence/task-21-memory-and-doc-migration.txt
  Commit: Y | docs(book): extract implementation lessons from superpowers plans

- [x] 23. Move docs/assets/ to doc/assets/
  What to do: Move the 4 gif.md files from `docs/assets/` to `doc/assets/`. Update any references in book chapters. Must NOT do: do not move the actual .gif files if they don't exist — only the .md wrappers.
  Parallelization: Wave 7 | Blocked by: none | Blocks: 23
  References: docs/assets/ (4 files); doc/book/*.md (check for references to assets/)
  Acceptance criteria: `doc/assets/` exists with the 4 files. No broken image references in `cargo doc` output.
  QA scenarios: (happy) assets render in docs; (failure) missing asset → broken link in cargo doc, fix path. Evidence: .omo/evidence/task-22-memory-and-doc-migration.txt
  Commit: Y | docs(book): move assets to doc/assets/

- [x] 24. Book restructuring: create new chapters + update doc/src/lib.rs
  What to do: After all knowledge extraction (todos 14-22) is done, create the new chapter files and update `doc/src/lib.rs` with new module includes. New chapters: memory_model.md, ipc.md, roadmap.md, audit.md, process_model.md, debugging.md, testing.md, interpreter_porting.md, gotchas.md. For each, add `#[doc = include_str!("../book/X.md")] pub mod X {}` to doc/src/lib.rs. Update the crate-level doc comment in lib.rs with links to new chapters. Must NOT do: do not change existing module names or ordering.
  Parallelization: Wave 8 | Blocked by: 14-22 | Blocks: 24
  References: doc/src/lib.rs (81 lines — add 9 new modules); doc/book/ (new files created by todos 14-22); doc/Cargo.toml (no changes needed)
  Acceptance criteria: `cargo doc --manifest-path doc/Cargo.toml` succeeds. All 22 chapters (13 existing + 9 new) render. No broken links.
  QA scenarios: (happy) `cargo doc` succeeds, 22 chapters in TOC; (failure) missing module → compile error, add it. Evidence: .omo/evidence/task-23-memory-and-doc-migration.txt
  Commit: Y | docs(book): add 9 new chapters and update lib.rs module includes

- [x] 25. Cross-reference updates (26+ files, not just README + AGENTS)
  What to do: Update ALL references from docs/ paths to doc/book/ paths. Metis found 26+ files reference docs/ paths — NOT just README.md and AGENTS.md. Full scope: (1) README.md — 5 refs; (2) AGENTS.md — 4 refs (including a broken ref to `gotchas/cluu-single-threaded-mutual-blocking-ipc-deadlock` which doesn't exist in docs/gotchas/ — fix or create); (3) .rs file comments (userspace/libcluu/src/ipc.rs, userspace/root-procmgr/src/main.rs, userspace/cluu_wire/src/pts.rs, others); (4) etc/envelopes.toml; (5) etc/architecture.txt, etc/welcome.txt (if they reference docs/); (6) python/cluu_harness/markers.py (if it references docs/); (7) doc/book/kernel.md and other book chapters that reference docs/ internally; (8) internal self-references within the 11 docs/ files themselves (update before deletion). Run `grep -rn 'docs/' --include='*.rs' --include='*.md' --include='*.toml' --include='*.py' --include='*.txt' --include='*.sh'` to find ALL remaining references. Must NOT do: do not update references in .claude/worktrees/ or .tmp/ — those are not shipped.
  Parallelization: Wave 9 | Blocked by: 23 | Blocks: 25
  References: README.md (5 refs to docs/); AGENTS.md (4 refs to docs/, 1 broken gotcha ref); etc/envelopes.toml; etc/architecture.txt; etc/welcome.txt; python/cluu_harness/markers.py; userspace/libcluu/src/ipc.rs; userspace/root-procmgr/src/main.rs; userspace/cluu_wire/src/pts.rs; doc/book/kernel.md (self-references to docs/); userspace/libcluu/src/lib.rs (mentions MicroPython); userspace/libcluu/src/posix/stubs.rs
  Acceptance criteria: `grep -rn 'docs/' --include='*.rs' --include='*.md' --include='*.toml' --include='*.py' --include='*.txt' --include='*.sh'` returns ZERO hits in tracked files (excluding .claude/worktrees/, .tmp/, .omo/, external/). `cargo xtask build` succeeds.
  QA scenarios: (happy) zero docs/ references in tracked files; (failure) missed reference → grep finds it, fix it. Evidence: .omo/evidence/task-24-memory-and-doc-migration.txt
  Commit: Y | docs: update all cross-references from docs/ to doc/book/

- [x] 26. docs/ retirement (GATED on cross-ref verification)
  What to do: Delete the entire `docs/` directory. This includes `docs/superpowers/` (66 files, 52K lines — nested inside docs/, NOT top-level). User decided: extract + git-history only — no archive directory. Originals survive in git history. VERIFY BEFORE DELETION: (1) `cargo xtask build` succeeds; (2) `cargo doc --manifest-path doc/Cargo.toml` succeeds; (3) `grep -rn 'docs/' --include='*.rs' --include='*.md' --include='*.toml' --include='*.py' --include='*.txt' --include='*.sh'` returns zero hits in tracked files (todo 24 must be COMPLETE); (4) all 22 book chapters render. Run `git status` to confirm docs/ deletion is staged. Must NOT do: do not delete doc/ (the book). Do not delete .omo/ plans. Do not delete docs/ until todo 24 cross-ref verification PASSES.
  Parallelization: Wave 10 | Blocked by: 24 (HARD GATE — cross-refs must be verified first) | Blocks: none
  References: docs/ (entire directory — 78+ files including docs/superpowers/); git history (originals preserved — `git log -- docs/` to access)
  Acceptance criteria: `docs/` does not exist. `cargo xtask build` succeeds. `cargo doc --manifest-path doc/Cargo.toml` succeeds. `grep -rn 'docs/'` returns zero hits in tracked files. SUPERPOWERS MANIFEST VERIFICATION: before deletion, create a manifest listing ALL 66 files in docs/superpowers/ (20 specs + 40+ plans + 2 audits + 2 designs + gotchas). For each file, the evidence file from todos 20-21 must record what was extracted (even if "no extractable lesson — historical implementation detail"). Zero files may be unaccounted. `find docs/superpowers -type f -name '*.md' | wc -l` must equal the manifest count.
  QA scenarios: (happy) docs/ gone, build + docs still work, zero broken refs, superpowers manifest 100% accounted; (failure) broken reference → grep finds it, RESTORE the needed file from git, fix the ref, re-delete; unaccounted superpowers file → extract its lesson before deleting. Evidence: .omo/evidence/task-25-memory-and-doc-migration.txt
  Commit: Y | docs: retire docs/ directory — knowledge migrated to doc/book/

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for the user's explicit okay before declaring complete.
- [x] F1. Plan compliance audit — verify every todo's acceptance criteria met; verify no Must-NOT-Have violated
- [x] F2. Code quality review — `cargo xtask build` clean, `cargo test --manifest-path userspace/libcluu/Cargo.toml --features host-test` passes (≥80/81), no new warnings. ADDITIONAL: grep new-code diff for `unwrap()` and `as any` (AGENTS.md §9 forbids these in new code) — `git diff --unified=0 | grep -E '^\+' | grep -E 'unwrap\(\)|as any'` must return zero hits.
- [x] F3. Real manual QA — boot CLUU via `cargo xtask run`, login, run `cat /proc/meminfo`, `top`, `micropython -c "print(2**64)"`, verify all work
- [x] F4. Scope fidelity — verify docs/ is gone, doc/book/ has ~22 chapters, all memory upgrades present in code

## Commit strategy
- Each todo gets its own commit (conventional commits format)
- No commits without explicit user request
- Code commits: `feat(kernel):`, `feat(userspace):`, `feat(allocator):`, etc.
- Doc commits: `docs(book):`
- Final retirement: `docs: retire docs/ directory`

## Success criteria
1. All 14 memory code upgrades implemented and verified via `cargo xtask build` + tests
2. doc/book/ expanded from 13 to ~22 chapters, all knowledge from docs/ extracted
3. `cargo doc --manifest-path doc/Cargo.toml` succeeds with all chapters
4. Zero references to docs/ in any tracked file
5. docs/ directory does not exist
6. CLUU boots and all existing harness tests pass
7. `/proc/meminfo` returns real values
8. No regressions in existing functionality

## Review findings folded in

### Metis gap analysis (8 critical findings)
1. **M1 already implemented** → todo 2 reclassified as verify + enhance (add per-process heap stats)
2. **M4 verification-only** → todo 4: run_key_destructors already runs 4 POSIX rounds
3. **C3 MAP_GUARD semantic undefined** → todo 1: semantic defined (not-present PTE, no frame allocated)
4. **M9 design gap** → todo 7: space_protect must be extended to allow unmapped pages (prerequisite added)
5. **M6 breaks fault handler** → todo 6: idt.rs:981 consumes global layout:: constants — must become per-process
6. **M16 mislabeled** → todo 9: rewritten from "new construction" to "verify existing + enforce refcount"
7. **Cross-ref scope underestimated** → todo 25: expanded from README+AGENTS to 26+ files
8. **M15 scope creep** → todo 14: reclassified as MAP_SHARED wrapper only, no shm_open

### Claude Code review pass 1 (isolated, no repo access — 3 critical + 5 medium + 1 low)
1. Wave conflicts (5 blocks 6, 9 blocks 10 in same wave) → dependency matrix corrected with Wave column
2. Superpowers coverage unverifiable → todo 26: superpowers manifest verification added
3. COW fork QA too shallow → todo 10: 5 QA scenarios (nested, concurrent, refcount-zero)
4. Todo 4 canary approach undecided → approach decided (procmgr-known stack_base)
5. Todo 12 bundles M12+M13 → split into todos 12 + 13
6. Todo 19 bundles 5 merges → split into 5 independent sub-tasks (19a-19e)
7. Todo 16 acceptance not binary → heading-extraction grep verification added
8. F2 no unwrap/as any check → F2 now includes grep for unwrap()/as any

### Claude Code review pass 2 (full repo access, Fable model — 1 critical + 1 medium + 1 low)
1. **M16 FrameTag already exists** → todo 9 rewritten: frame_table.rs:81-89 has all 7 variants, inc_ref/dec_ref exist, SpaceDestroy calls dec_ref. Gap is "Phase 1 advisory → Phase 2 enforced" only.
2. Metis findings traceability → this appendix added (8 findings listed above with todo mapping)
3. idt.rs:981 wording → todo 6: "consumes global layout:: constants" not "hardcodes literal numbers"

### Momus review (full repo access — OKAY, 5 advisory notes)
1. fault.rs:284 is test code → todo 6 updated to note this
2. Todo 13 vs 15 ordering → flexibility via "or" in acceptance criteria
3. Todo 11 internal inconsistency → todo 11 clarified: handle_heap_fault already demand-pages stack region
4. Todo 10 COW fork API unspecified → todo 10: composes existing invoke ops, no new syscall
5. Subjective doc-migration criteria → todos 14/16 now have grep/heading-count checks
