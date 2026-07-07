# Memory Model

CLUU gives each process a strict, isolated address space. The kernel owns
frame allocation and page tables; userspace owns its heap, stack, and mmap
regions. This chapter covers the layout, the allocators, demand paging, and
the guard-page invariants that make NULL derefs and stack overflows
recoverable.

See [The Kernel](../kernel/index.html) for the PMM/VMM implementations and
[Capability Tokens](../capability_tokens/index.html) for the `SpaceMap` /
`SpaceProtect` invoke ops that drive these mappings.

## Address space layout

Every process gets the same fixed layout, randomized at spawn by M6 ASLR.
Heap and mmap starts take a per-process page-aligned random offset (heap:
~15-bit entropy over a 128 MB range, `HEAP_ASLR_RANGE`,
`posix/memory.rs:24`; mmap: same 128 MB range, `MMAP_ASLR_RANGE`,
`posix/memory.rs:50`). Stack ASLR randomizes the per-thread stack top inside
the 16 MiB stack region (~12-bit entropy over a 15 MiB range,
`STACK_ASLR_RANGE`, `userspace/init/src/wiring.rs:58`); the kernel records
the resulting guard-page boundary per process in
`AddressSpace::aslr_stack_guard_end` (`kernel/src/mm/space.rs:251`) so the
page-fault handler can distinguish demand-faults below the randomized stack
top from genuine overflow (`idt.rs:976-981`).

```text
USERSPACE
0x00000000  NULL guard (unmapped, 4 KiB)
0x00400000  text
0x00600000  data / BSS
0x00800000  heap (lazy; Rust linked-list or newlib _sbrk)
0x41000000  mmap region (240 MiB, dynamic Vec-backed region table)
0x50000000  end of mmap region
0x60000000  pthread stack region (256 MiB, 0x6000_0000..0x7000_0000)
0x6f000000  main process stack (64 KiB, mapped by procmgr)
0x7f000000  kernel stack region (16 MiB, demand-paged; procmgr maps 64 KiB)
0x80000000  USER_STACK_TOP

KERNELSPACE (high half)
0xffff8000_00000000  physmap
0xffffffff_c0000000  kernel heap
```

Three properties hold across the whole layout:

- Heap allocation is lazy via page faults.
- Every user pointer is validated before the kernel touches it.
- No memory is implicitly shared between processes.

## Heap

The heap lives at `0x0080_0000..0x0400_0000`. Two allocators serve it, picked
at compile time by a cargo feature. They never coexist in one binary.

### Rust path (default)

`LinkededListAllocator` with free-block coalescing
(`userspace/libcluu/src/allocator.rs:193-482`). It starts at 256 KiB
(`INITIAL_HEAP_SIZE`, `allocator.rs:134`), grows in steps of 256 KiB up to
16 MiB (`MIN_HEAP_GROW` / `MAX_HEAP_GROW`, `allocator.rs:136-140`), and tops
out at 1 GiB (`USER_HEAP_MAX = 0x0400_0000`, `allocator.rs:160`).

A 64 KiB static bootstrap heap (`STATIC_HEAP_SIZE`, `allocator.rs:131`)
serves early allocations before boot tokens exist.

### C-runtime path (feature-gated)

With the `c-runtime` feature, `NewlibAllocator` delegates to `malloc` / `free`
(`allocator.rs:54-118`). newlib initializes itself through `_sbrk`
(`userspace/libcluu/src/posix/memory.rs:592-649`), using the same
`0x0080_0000..0x4000_0000` region (`HEAP_START` / `HEAP_MAX`,
`posix/memory.rs:58-61`). The feature selects which allocator is
`#[global_allocator]`.

## Stack

### Main process stack

64 KiB, fixed, no growth. `PROC_STACK_SIZE = 64 * 1024`
(`userspace/init/src/wiring.rs:121`). procmgr maps it through `map_stack`
(`userspace/libcluu/src/process.rs:120`) with a single `space_map_range`
call.

The C1 guard-page upgrade leaves a 4 KiB guard page unmapped below the stack
base. Overflow trips a page fault that kills the thread
(`kernel/src/architecture/x86_64/idt.rs:955-957`).

The kernel reserves a 16 MiB stack *region* (`USER_STACK_SIZE = 16 * 1024 *
1024`, `kernel/src/mm/space.rs:52`; top at `USER_STACK_TOP = 0x8000_0000`)
even though procmgr only maps 64 KiB up front. The reserved-but-unmapped
span is demand-paged by `handle_heap_fault` (`idt.rs:967-1039`), so a
recursive call that walks past the initial 64 KiB still works up to the 16
MiB ceiling.

### Pthread stacks

64 KiB (16 pages, `DEFAULT_STACK_PAGES = 16`,
`userspace/libcluu/src/posix/pthread.rs:108`), allocated from
`0x6000_0000..0x7000_0000` (`THREAD_STACK_REGION_START` / `_END`,
`pthread.rs:113-114`). Each thread gets a guard page below its stack:
`alloc_thread_stack` (`pthread.rs:213-235`) leaves `stack_base - PAGE_SIZE`
unmapped, so overflow page-faults instead of corrupting the neighbor.

## mmap region

`0x4100_0000..0x5000_0000` (240 MiB). Tracked regions live in a
`MmapRegionTable` backed by a `Vec<MmapRegion>` (`posix/memory.rs:79-140`),
grown on demand via `try_reserve` — there is no fixed upper bound. A
first-fit allocator walks the tracked regions so freed holes get reused. This is the natural path for an interpreter that
needs a big contiguous GC heap via `mmap(MAP_ANONYMOUS)`. See
[Interpreter Porting](../interpreter_porting/index.html) for the pattern.

### MAP_SHARED wrapper (MAP_SHARE_PHYS)

The kernel's `MAP_SHARE_PHYS` flag (`0x800`, `handlers.rs` invoke_space_map_range)
remaps the caller's physical frames backing a source virtual address into a
target address space, always read-only. It is the primitive for sharing a
physical frame between two processes by agreement. No `shm_open`/`shm_unlink`
and no `/dev/shm` filesystem — the wrapper is the mmap path plus the
existing space_map_range invoke op.

Two calling conventions in `mmap` (`posix/memory.rs`):

- `mmap(NULL, len, prot, MAP_SHARED|MAP_ANONYMOUS, -1, 0)` — same as
  `MAP_PRIVATE|MAP_ANONYMOUS`: allocates a new writable anonymous region the
  caller can later share. No sharing happens yet.
- `mmap(NULL, len, prot, MAP_SHARED|MAP_ANONYMOUS, -1, src_virt)` with
  `src_virt` page-aligned and non-zero — routes to `space_map_range` with
  `MAP_SHARE_PHYS`. The kernel looks up the physical frames backing
  `src_virt` in the caller's page table and maps them read-only at a new
  address in the mmap region. `PROT_WRITE` is silently dropped (the kernel
  ignores the writable bit for `MAP_SHARE_PHYS`).

Cross-process sharing requires the owner to hold a token for the receiver's
space (obtained via IPC). The owner calls `space_map_range` directly with
the receiver's space token, the owner's source VA as `source_ptr`, and
`MAP_SHARE_PHYS`:

```text
owner:  mmap(NULL, len, RW, MAP_ANONYMOUS, -1, 0)  ->  writable region at VA
owner:  [write data into VA]
owner:  space_map_range(receiver_token, recv_virt, VA, MAP_SHARE_PHYS, npages, len)
        // kernel maps owner's frames read-only into receiver at recv_virt
receiver: reads from recv_virt
```

`mmap` itself only holds the caller's own space token, so the cross-process
step uses `space_map_range` directly rather than the `mmap` wrapper. The
wrapper covers the same-space alias case (a read-only view of a writable
region within one process) and is the documented entry point for the
`MAP_SHARED` POSIX flag. The VFS uses `MAP_SHARE_PHYS` the same way to map
ELF text segments from its page cache into spawned processes
(`userspace/vfs/src/main.rs`).

The compositor's cell-grid SHM uses `MAP_FRAME_TOKEN` (a different kernel
flag that maps a frame-token-identified frame rather than a source-VA
region); see [Terminal](../terminal/index.html). Both flags share the same
`space_map_range` dispatch path. The historical `MAP_SHARE_PHYS` use-after-
free (documented in the retired roadmap, now `doc/book/roadmap.md`) is avoided by keeping the
source pages mapped for as long as any `MAP_SHARE_PHYS` alias exists;
`munmap` of the source does not free the physical frames while aliases hold
them.

## Demand paging and fault handling

Physical frames are allocated lazily on page fault for the heap and stack
regions (`handle_heap_fault`, `idt.rs:967-1039`). The handler:

1. `try_alloc_frame`s a free frame from the PMM.
2. Zeroes it through the physmap.
3. Maps it RW user.

The guard page at the bottom of the stack region is explicitly *not*
demand-paged (`idt.rs:979-982`), so stack overflow into it is unrecoverable
rather than silently extending the stack forever.

Faults outside the heap and stack regions, with no registered fault endpoint,
kill the thread. Faults with a registered endpoint are forwarded; see
[Fault forwarding](../kernel/index.html#fault-forwarding-threadsetfaultendpoint).

## Guard pages

Three guard regions protect the address space:

- **NULL guard** at `0x00000000..0x00001000` (below). NULL dereferences trap
  instead of corrupting page 0.
- **Stack guard** below the main process stack (C1 upgrade) and below every
  pthread stack. Overflow kills the thread.
- **MAP_GUARD** (C3) lets a binary place an explicit guard page anywhere in
  the address space via `space_map_range`. The kernel installs a not-present
  PTE with no backing frame; access faults and is either forwarded (if a
  fault endpoint is registered) or kills the thread.

## NULL guard

`0x00000000..0x00001000` is unmapped in every process. This is process
isolation, not a stack guard: NULL-pointer dereferences trap instead of
writing page 0.

## Allocator re-entrancy caveat

`LockedAllocator::dealloc` (`allocator.rs:530-536`) uses `try_lock`. On a
re-entrant free (for example, a GC triggered during an allocation that tries
to free unreachable objects), the free leaks. The code says so directly:

```text
// Avoid deadlock on re-entrant free; leak as a safe fallback.
```

The C2 upgrade adds a deferred-free list to fix this. See
[Gotchas](../gotchas/index.html) for the allocator-reentrancy-leak entry.

## Documentation findings

### F-008 — Memory model was undocumented (resolved)

The original internals doc's Memory Model section was 4 lines of address
map and 3 bullets. User stack size, heap growth, the two allocator paths (Rust
linked-list vs newlib `_sbrk`), the `fault_endpoint` mechanism, the mmap
region, pthread stack guard pages, and the allocator re-entrancy leak were all
absent from the book. This chapter, plus the
[Capability Tokens](../capability_tokens/index.html#spaceprotect-semantics)
SpaceProtect semantics, the
[Kernel](../kernel/index.html#fault-forwarding-threadsetfaultendpoint) fault
forwarding section, [Interpreter Porting](../interpreter_porting/index.html),
and [Gotchas](../gotchas/index.html#allocator-reentrancy-leak), now cover the
full memory model.
