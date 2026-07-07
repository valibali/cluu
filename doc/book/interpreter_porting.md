# Porting an Interpreter to CLUU

A guide for porting interpreters that need GC (Python, Lua, Ruby, JS, etc.)
to CLUU. Written during the memory-model audit (2026-07-07) alongside the
C1 through C5 memory upgrades.

See [Memory Model](../memory_model/index.html) for the address space layout
and [The Kernel](../kernel/index.html#fault-forwarding-threadsetfaultendpoint)
for the fault forwarding primitive that enables write-barrier GC.

## The canonical pattern

Every interpreter that wants GC ports the same way MicroPython did: declare a
static array or `mmap` a chunk once, run the interpreter's own GC inside that
region, and use `malloc` only for non-GC things (interpreter metadata, file
buffers, strings the GC does not own).

From `userspace/micropython/main.c:25-27`:

```c
#define HEAP_SIZE (1024 * 1024)  // 1MB GC heap

static char heap[HEAP_SIZE];
```

And `userspace/micropython/main.c:76-79`:

```c
    mp_stack_ctrl_init();
    mp_stack_set_limit(40000);

    // GC heap
    gc_init(heap, heap + sizeof(heap));
```

The static `heap[]` is the GC heap. `gc_init` gets its bounds. `malloc` is
never called for GC-managed objects. This sidesteps every CLUU allocator
caveat (re-entrancy leak, fragmentation, region limits) because the GC owns
a contiguous span the global allocator never touches.

## CLUU memory model recap

See [Memory Model](../memory_model/index.html) for the full map. The numbers
that matter to an interpreter:

- **Stack:** 64 KB, fixed, no growth. `mp_stack_set_limit(40000)` is safe
  (leaves headroom for the C frames between the runtime entry and the
  interpreter's stack check).
- **Heap:** 1 GiB max (`USER_HEAP_MAX = 0x0400_0000`). Rust linked-list
  allocator (default) or newlib `_sbrk` (c-runtime feature). The two are
  feature-gated, never coexisting.
- **mmap region:** 240 MB at `0x4100_0000..0x5000_0000`, max 64 tracked
  regions, first-fit. The natural path for a big contiguous GC heap via
  `mmap(MAP_ANONYMOUS)`.
- **pthread stacks:** 64 KB with a guard page below, allocated from
  `0x6000_0000..0x7000_0000`.

## GC options on CLUU

Three patterns, in order of recommendation:

**(a) Bring-your-own-heap (MicroPython style, recommended).** Declare a
static array or `mmap` a chunk, hand its bounds to the interpreter's GC, and
never route GC-managed objects through `malloc`. Simplest, most robust,
avoids the re-entrancy leak (see [Gotchas](../gotchas/index.html#allocator-reentrancy-leak))
entirely.

**(b) Conservative scan over malloc'd blocks.** Hand the allocator's
allocation list to a conservative GC. Works, but false positives retain
garbage (any word-looking-like-a-pointer pins a block), and fragmentation in
the linked-list allocator accumulates over long runs. Acceptable for
short-lived programs, fragile for daemons.

**(c) Write-barrier GC via `fault_endpoint` + `space_protect` (advanced).**
Enables generational or copying GC. Register a fault endpoint on each
mutator thread (`InvokeOp::ThreadSetFaultEndpoint`, see
[The Kernel](../kernel/index.html#fault-forwarding-threadsetfaultendpoint)),
`space_protect` a page read-only, and on write fault the GC handler logs the
cross-generational pointer, unprotects the page, and replies to resume the
mutator. This is the only path that gives precise write barriers; it costs a
page fault per barrier crossing. Requires the C5 `mprotect(PROT_NONE)`
upgrade.

## What CLUU does NOT provide

- **No `mprotect`-after-map in the default allocator.** `space_protect`
  exists at the kernel invoke-op level (`SpaceProtect`, see
  [Capability Tokens](../capability_tokens/index.html#spaceprotect-semantics)),
  but neither the Rust linked-list allocator nor newlib `_sbrk` calls it. A
  write-barrier GC must call `mprotect` itself.
- **No stack growth.** 64 KB is fixed. An interpreter that recurses deeply
  must implement its own stack-overflow check (MicroPython's
  `mp_stack_set_limit`) or run on an explicit stack allocated from the heap.
- **No memory-pressure callback.** Until the C4 upgrade lands, there is no
  way for the allocator to notify a process that it is near OOM. An
  interpreter must poll or set its own soft limit.
- **No allocator-GC cooperation in the default allocator.** The Rust
  linked-list allocator does not expose a "walk all live blocks" primitive.
  Pattern (b) above requires either a separate tracking layer or the
  deferred-free list from C2.

## Reference port: MicroPython

Source lives in two places:

- `userspace/micropython/` — the CLUU-side wrapper: `main.c` (entry, stack
  limit, GC init, arg parsing), the `Makefile` fragment, and any CLUU-specific
  patches.
- `external/micropython/` — the upstream MicroPython tree (not tracked in
  git; fetched by the build).

The xtask build entry point is `build_micropython`
(`xtask/src/main.rs:2898`), invoked from the main build sequence. It compiles
MicroPython with the CLUU POSIX shim (`userspace/libcluu/src/posix/`) as the
C runtime, links against newlib, and stages the resulting ELF as a container
(`/bin/micropython`).

The porting surface is small:

1. `main.c` declares the static `heap[HEAP_SIZE]` and calls
   `gc_init(heap, heap + sizeof(heap))`.
2. `mp_stack_set_limit(40000)` caps interpreter recursion against the 64 KB
   stack.
3. File I/O goes through the POSIX shim (`open`/`read`/`write`/`close` map
   to VFS IPC).
4. `malloc`/`free` (newlib via `_sbrk`) are used only for non-GC allocations.

## Porting checklist

- [ ] Pick a heap strategy: (a) static/mmap owned by the interpreter's GC,
      (b) conservative scan over `malloc`, or (c) write-barrier via
      `fault_endpoint` + `space_protect`.
- [ ] Set a stack limit below 64 KB. Account for the C frames between the
      runtime entry and the interpreter's own stack check.
- [ ] Decide single vs multi-threaded. Multi-threaded needs pthreads
      (`libcluu::posix::pthread`), which gives 64 KB stacks with guard pages.
- [ ] Decide a GC trigger: manual (`gc_collect`-style entry point),
      OOM-callback (after C4 lands), or write-barrier (via `fault_endpoint`).
- [ ] If using `mmap` for the GC heap, stay under 240 MB total and under 64
      concurrent regions.
- [ ] If using pattern (b) or (c), read
      [Gotchas](../gotchas/index.html#allocator-reentrancy-leak) before
      relying on `free` from inside a GC callback.
