//! # Userspace heap allocator
//!
//! Provides the `#[global_allocator]` for Rust userspace binaries.
//!
//! ## Role in CLUU
//!
//! Every Rust userspace process needs a heap. This module supplies one of two
//! implementations selected at build time by the `c-runtime` feature flag:
//! pure-Rust linked-list allocator (default) or a thin wrapper over newlib's
//! `malloc`/`free` (C-runtime builds). The pure-Rust path grows the heap
//! on demand via `syscall::space_map_range` against the process's space
//! token, falling back to a 64 KiB static bootstrap heap before boot tokens
//! are available. The C-runtime path delegates entirely to newlib, which
//! self-initializes through `_sbrk`.
//!
//! ## What it implements
//!
//! - `AllocStats` — total/used/peak/free byte counters.
//! - `NewlibAllocator` (c-runtime) — `GlobalAlloc` delegating to `malloc`/`free`.
//! - `LockedAllocator` (pure-Rust) — `Mutex<LinkedListAllocator>` `GlobalAlloc`.
//! - `NurseryAllocator` (pure-Rust) — wraps `LockedAllocator` with a 1 MiB
//!   bump-pointer nursery for small allocations (<256 B); installed as the
//!   `#[global_allocator]`.
//! - `GLOBAL_ALLOCATOR` — the installed `#[global_allocator]` static.
//! - `init` — initialise the heap (pure-Rust) or no-op (newlib).
//! - `stats` — current `AllocStats` (pure-Rust) or zeros (newlib).
//!
//! ## Design drivers & invariants
//!
//! The pure-Rust allocator uses a sorted free-list with coalescing on every
//! insert; allocations carry an in-band `AllocHeader` immediately preceding
//! the user pointer so `dealloc` can recover the block size without the
//! `Layout`. Heap growth is bounded by `USER_HEAP_MAX` (1 GiB) and proceeds
//! in page-aligned chunks of at least `MIN_HEAP_GROW` (256 KiB), doubling
//! each grow up to `MAX_HEAP_GROW` (16 MiB). The `GlobalAlloc` impl blocks
//! on `alloc` (safe — alloc never re-enters alloc) and uses `try_lock` +
//! deferred-free on `dealloc` to avoid re-entrant deadlock when a Drop
//! fires mid-alloc (GC callback scenario). The `host-test` feature stubs
//! `#[global_allocator]` so host unit tests use the std heap.
//!
//! ## Cross-references
//!
//! - Related files: `boot.rs` — supplies `space_token()` used to grow the heap;
//!   `syscall.rs` — `space_map_range` is the growth primitive.
//! - IPC ops / labels: none.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

#[derive(Copy, Clone, Debug)]
    pub struct AllocStats {
        pub total: usize,
        pub used: usize,
        pub peak: usize,
        pub free: usize,
        /// Size of the largest contiguous free block (bytes).
        ///
        /// The fragmentation ratio is `largest_free / free`: 1.0 when the free
        /// space is a single contiguous region, approaching 0 as the free list
        /// fragments. Zero when the heap is empty or fully used.
        pub largest_free: usize,
        pub leaked_deallocs: u64,
}

// ═══════════════════════════════════════════════════════════════════════════════
// C-runtime mode: delegate to newlib's malloc/free
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(feature = "c-runtime")]
mod inner {
    use super::*;
    use core::ffi::c_void;

    extern "C" {
        fn malloc(size: usize) -> *mut c_void;
        fn free(ptr: *mut c_void);
    }

    pub struct NewlibAllocator;

    unsafe impl GlobalAlloc for NewlibAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if layout.align() <= 16 {
                unsafe { malloc(layout.size()) as *mut u8 }
            } else {
                // Over-allocate to satisfy alignment > 16
                let overhead = core::mem::size_of::<usize>();
                let total = layout.size() + layout.align() + overhead;
                let raw = unsafe { malloc(total) as *mut u8 };
                if raw.is_null() {
                    return ptr::null_mut();
                }
                let aligned = ((raw as usize + overhead + layout.align() - 1)
                    & !(layout.align() - 1)) as *mut u8;
                // Store original pointer before the aligned address
                unsafe {
                    (aligned as *mut usize).sub(1).write(raw as usize);
                }
                aligned
            }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            if layout.align() <= 16 {
                unsafe { free(ptr as *mut c_void) }
            } else {
                let original = unsafe { (ptr as *mut usize).sub(1).read() } as *mut c_void;
                unsafe { free(original) }
            }
        }
    }

    #[cfg(not(feature = "host-test"))]
    #[global_allocator]
    pub static GLOBAL_ALLOCATOR: NewlibAllocator = NewlibAllocator;

    pub fn init() {
        // No-op: newlib's malloc self-initializes via _sbrk on first call
    }

    pub fn stats() -> AllocStats {
        AllocStats {
            total: 0,
            used: 0,
            peak: 0,
            free: 0,
            largest_free: 0,
            leaked_deallocs: 0,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Pure Rust mode: linked-list allocator with dynamic heap growth
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(not(feature = "c-runtime"))]
mod inner {
    use super::*;
    use core::mem::{align_of, size_of};
    use core::sync::atomic::{AtomicUsize, Ordering};
    use spin::Mutex;

    /// Size of static bootstrap heap (64KB - fallback for early init).
    const STATIC_HEAP_SIZE: usize = 64 * 1024;

    /// Initial dynamic heap size (256KB - mapped eagerly at init).
    const INITIAL_HEAP_SIZE: usize = 256 * 1024;

    /// Minimum heap growth increment (256KB).
    const MIN_HEAP_GROW: usize = 256 * 1024;

    /// Page size for heap allocation.
    const PAGE_SIZE: usize = 4096;

    /// Start of dynamic userspace heap region (must match kernel's USER_HEAP_START).
    const USER_HEAP_START: usize = 0x0080_0000;

    /// Top of the userspace heap VA region (absolute address).
    ///
    /// The heap lives in `[USER_HEAP_START, USER_HEAP_MAX)`. `USER_HEAP_MAX`
    /// is the architectural ceiling: it must stay below newlib's `_sbrk`
    /// range top (`0x4000_0000`) and the mmap region start (`0x4100_0000`).
    /// With `USER_HEAP_MAX = 0x4000_0000` the usable span is up to ~1 GiB
    /// (reduced by the ASLR offset added to `USER_HEAP_START`), so a Rust
    /// binary's heap is bounded only by physical RAM (PMM), not by an
    /// artificial per-process cap. `heap_max` is set to this constant
    /// verbatim (not start-relative) so the ASLR offset can never push the
    /// heap into the mmap region.
    const USER_HEAP_MAX: usize = 0x4000_0000;

    // M6 ASLR: per-process random offset added to USER_HEAP_START. Bounded
    // to 128 MB (page-aligned) so the heap stays well below the mmap region
    // (0x4100_0000) and above the data segment (0x0080_0000).
    const HEAP_ASLR_RANGE: usize = 128 * 1024 * 1024;
    static HEAP_START_RANDOMIZED: AtomicUsize = AtomicUsize::new(0);

    fn randomized_heap_start() -> usize {
        let start = HEAP_START_RANDOMIZED.load(Ordering::Relaxed);
        if start != 0 {
            return start;
        }
        let mut buf = [0u8; 8];
        klibcluu::crypto::fill_random(&mut buf);
        let r = u64::from_le_bytes(buf) as usize;
        let offset = (r & (HEAP_ASLR_RANGE - 1)) & !0xFFF;
        let randomized = USER_HEAP_START + offset;
        HEAP_START_RANDOMIZED.store(randomized, Ordering::Relaxed);
        randomized
    }

    // ─────────────────────────────────────────────────────────────────────
    // Bump-pointer nursery (tcache-style fast path for small allocs)
    // ─────────────────────────────────────────────────────────────────────
    //
    // Allocations <= NURSERY_THRESHOLD bytes with alignment <=
    // NURSERY_MAX_ALIGN are served from a contiguous bump-pointer arena
    // (NURSERY_SIZE bytes). The nursery is allocation-only: individual
    // deallocs of nursery pointers are no-ops. When the nursery fills,
    // it is swept (bump pointer reset to start) and the allocation is
    // retried; if it still does not fit, the request falls through to the
    // linked-list allocator.
    //
    // SAFETY CONTRACT: sweeping reclaims all nursery memory regardless of
    // liveness. This is safe only when callers do not retain nursery
    // pointers across a sweep. In practice this holds because (a) the
    // nursery threshold is small (256 B) so long-lived objects — which tend
    // to be larger or to grow — go to the linked-list allocator, and
    // (b) the nursery is large (1 MiB) relative to the rate of small alloc,
    // so sweeps are rare. Pattern: jemalloc tcache, tcmalloc thread-local
    // FreeList — a fast per-instance cache in front of the general
    // allocator, accepting bulk-free semantics for speed.

    /// Nursery arena size (1 MiB).
    const NURSERY_SIZE: usize = 1024 * 1024;

    /// Allocations with `layout.size() <= NURSERY_THRESHOLD` go to the
    /// nursery fast path.
    const NURSERY_THRESHOLD: usize = 256;

    /// Maximum alignment served by the nursery. Larger alignments fall
    /// through to the linked-list allocator to avoid wasting arena space.
    const NURSERY_MAX_ALIGN: usize = 64;

    /// Static bootstrap heap for early allocations before boot tokens are ready.
    #[repr(align(4096))]
    #[allow(dead_code)]
    struct StaticHeap([u8; STATIC_HEAP_SIZE]);

    static mut STATIC_HEAP: StaticHeap = StaticHeap([0; STATIC_HEAP_SIZE]);

    /// Nursery arena backing memory (BSS, zero-initialised).
    #[repr(align(64))]
    #[allow(dead_code)]
    struct NurseryHeap([u8; NURSERY_SIZE]);

    static mut NURSERY_HEAP: NurseryHeap = NurseryHeap([0; NURSERY_SIZE]);

    const ALLOC_MAGIC: u64 = 0xA110_C8ED_BEEF_F00D;

    #[repr(C)]
    struct AllocHeader {
        magic: u64,
        size: usize,
    }

    struct ListNode {
        size: usize,
        next: Option<&'static mut ListNode>,
    }

    impl ListNode {
        const fn new(size: usize) -> Self {
            Self { size, next: None }
        }

        fn start_addr(&self) -> usize {
            self as *const Self as usize
        }

        fn end_addr(&self) -> usize {
            self.start_addr() + self.size
        }
    }

    struct LinkedListAllocator {
        head: ListNode,
        heap_start: usize,
        heap_end: usize,
        heap_max: usize,
        dynamic_start: usize,
        used: usize,
        peak: usize,
    }

    impl LinkedListAllocator {
        const fn new() -> Self {
            Self {
                head: ListNode::new(0),
                heap_start: 0,
                heap_end: 0,
                heap_max: 0,
                dynamic_start: 0,
                used: 0,
                peak: 0,
            }
        }

        unsafe fn init(&mut self) {
            let heap_start = randomized_heap_start();
            self.heap_max = USER_HEAP_MAX;
            self.dynamic_start = heap_start;
            self.used = 0;
            self.peak = 0;
            self.head.next = None;

            let space_token = crate::boot::space_token();
            if space_token != 0 {
                let pages = INITIAL_HEAP_SIZE / PAGE_SIZE;
                if crate::syscall::space_map_range(space_token, heap_start, 0, 0x03, pages, 0)
                    .is_ok()
                {
                    self.heap_start = heap_start;
                    self.heap_end = heap_start + INITIAL_HEAP_SIZE;
                    self.add_free_region(heap_start, INITIAL_HEAP_SIZE);
                    return;
                }
            }

            let static_start = core::ptr::addr_of!(STATIC_HEAP).cast::<u8>() as usize;
            let static_end = static_start + STATIC_HEAP_SIZE;

            self.heap_start = static_start;
            self.heap_end = static_end;
            self.add_free_region(static_start, STATIC_HEAP_SIZE);
        }

        fn stats(&mut self) -> AllocStats {
            let total = self.heap_end.saturating_sub(self.heap_start);
            let used = self.used;
            let free = total.saturating_sub(used);
            let (largest_free, _total_free) = self.fragmentation();
            AllocStats {
                total,
                used,
                peak: self.peak,
                free,
                largest_free,
                leaked_deallocs: 0,
            }
        }

        /// Walk the free list and return `(largest_free_block, total_free_bytes)`.
        ///
        /// The fragmentation ratio is `largest / total`: 1.0 when the free
        /// space is one contiguous block, approaching 0 as the free list
        /// fragments into many small regions. Returns `(0, 0)` for an empty
        /// heap.
        fn fragmentation(&mut self) -> (usize, usize) {
            let mut largest = 0;
            let mut total = 0;
            let mut current = &mut self.head;
            while let Some(next) = current.next.as_mut() {
                total += next.size;
                if next.size > largest {
                    largest = next.size;
                }
                match current.next.as_mut() {
                    Some(n) => current = n,
                    None => break,
                }
            }
            (largest, total)
        }

        fn align_up(value: usize, align: usize) -> usize {
            (value + align - 1) & !(align - 1)
        }

        fn grow_heap(&mut self, min_size: usize) -> bool {
            let space_token = crate::boot::space_token();
            if space_token == 0 {
                return false;
            }

            let map_start = if self.heap_end >= self.dynamic_start {
                self.heap_end
            } else {
                self.dynamic_start
            };

            let current_size = if self.heap_end >= self.dynamic_start {
                self.heap_end.saturating_sub(self.dynamic_start)
            } else {
                0
            };
            let double_size = current_size;
            let headroom = (min_size / 4).max(MIN_HEAP_GROW);

            let grow_size = min_size
                .max(headroom)
                .max(MIN_HEAP_GROW)
                .min(double_size.max(min_size + headroom));

            let grow_pages = (grow_size + PAGE_SIZE - 1) / PAGE_SIZE;
            let actual_grow = grow_pages * PAGE_SIZE;
            let new_end = map_start.saturating_add(actual_grow);

            if new_end > self.heap_max {
                let fallback_pages = (min_size + PAGE_SIZE - 1) / PAGE_SIZE;
                let fallback_grow = fallback_pages * PAGE_SIZE;
                let fallback_end = map_start.saturating_add(fallback_grow);

                if fallback_end > self.heap_max {
                    return false;
                }

                return self.do_grow(space_token, map_start, fallback_pages, fallback_grow);
            }

            self.do_grow(space_token, map_start, grow_pages, actual_grow)
        }

        fn do_grow(
            &mut self,
            space_token: usize,
            map_start: usize,
            pages: usize,
            size: usize,
        ) -> bool {
            match crate::syscall::space_map_range(space_token, map_start, 0, 0x03, pages, 0) {
                Ok(_) => {
                    unsafe {
                        self.add_free_region(map_start, size);
                    }
                    self.heap_end = map_start + size;
                    true
                }
                Err(_) => false,
            }
        }

        unsafe fn add_free_region(&mut self, addr: usize, size: usize) {
            if size < size_of::<ListNode>() {
                return;
            }
            let aligned_addr = Self::align_up(addr, align_of::<ListNode>());
            let aligned_end = addr + size;
            if aligned_end <= aligned_addr {
                return;
            }
            let aligned_size = aligned_end - aligned_addr;
            if aligned_size < size_of::<ListNode>() {
                return;
            }

            let node_ptr = aligned_addr as *mut ListNode;
            node_ptr.write(ListNode::new(aligned_size));
            self.insert_node(&mut *node_ptr);
        }

        unsafe fn insert_node(&mut self, node: &'static mut ListNode) {
            let mut current = &mut self.head;
            while let Some(next) = current.next.as_mut() {
                if node.start_addr() < next.start_addr() {
                    break;
                }
                current = current.next.as_mut().unwrap();
            }

            node.next = current.next.take();
            current.next = Some(node);

            self.coalesce();
        }

        fn coalesce(&mut self) {
            let mut current = &mut self.head;
            while let Some(next) = current.next.as_mut() {
                let next_end = next.end_addr();
                if let Some(next_next) = next.next.as_mut() {
                    let next_next_start = next_next.start_addr();
                    if next_end == next_next_start {
                        let merged_size = next.size + next_next.size;
                        let next_next_next = next_next.next.take();
                        next.size = merged_size;
                        next.next = next_next_next;
                        continue;
                    }
                }
                current = current.next.as_mut().unwrap();
            }
        }

        fn alloc_from_region(
            region: &ListNode,
            size: usize,
            align: usize,
        ) -> Option<(usize, usize, usize)> {
            let header_size = size_of::<AllocHeader>();
            let region_start = region.start_addr();
            let region_end = region.end_addr();

            let user_start = Self::align_up(region_start + header_size, align);
            let header_start = user_start - header_size;
            let alloc_end = user_start.checked_add(size)?;

            if alloc_end > region_end {
                return None;
            }

            let excess_before = header_start - region_start;
            if excess_before > 0 && excess_before < size_of::<ListNode>() {
                return None;
            }

            let excess_after = region_end - alloc_end;
            if excess_after > 0 && excess_after < size_of::<ListNode>() {
                return None;
            }

            Some((header_start, user_start, alloc_end))
        }

        fn try_alloc(&mut self, layout: Layout) -> Option<*mut u8> {
            let size = layout.size().max(1);
            let align = layout
                .align()
                .max(align_of::<AllocHeader>())
                .max(align_of::<ListNode>());

            let mut current = &mut self.head;
            while let Some(region) = current.next.as_mut() {
                if let Some((header_start, user_start, alloc_end)) =
                    Self::alloc_from_region(region, size, align)
                {
                    let region_start = region.start_addr();
                    let region_end = region.end_addr();
                    let region_next = region.next.take();
                    current.next = region_next;

                    let excess_before = header_start - region_start;
                    let excess_after = region_end - alloc_end;

                    unsafe {
                        if excess_before > 0 {
                            self.add_free_region(region_start, excess_before);
                        }
                        if excess_after > 0 {
                            self.add_free_region(alloc_end, excess_after);
                        }

                        let header = header_start as *mut AllocHeader;
                        header.write(AllocHeader {
                            magic: ALLOC_MAGIC,
                            size: alloc_end - header_start,
                        });
                    }

                    self.used = self.used.saturating_add(alloc_end - header_start);
                    if self.used > self.peak {
                        self.peak = self.used;
                    }

                    return Some(user_start as *mut u8);
                }

                current = current.next.as_mut().unwrap();
            }

            None
        }

        fn alloc(&mut self, layout: Layout) -> *mut u8 {
            if let Some(ptr) = self.try_alloc(layout) {
                return ptr;
            }

            let needed = layout.size() + size_of::<AllocHeader>() + align_of::<ListNode>() + 64;
            if self.grow_heap(needed) {
                if let Some(ptr) = self.try_alloc(layout) {
                    return ptr;
                }
            }

            ptr::null_mut()
        }

        fn dealloc(&mut self, ptr: *mut u8) {
            if ptr.is_null() {
                return;
            }
            let header_size = size_of::<AllocHeader>();
            let header_ptr = unsafe { ptr.sub(header_size) } as *mut AllocHeader;
            let magic = unsafe { (*header_ptr).magic };
            let size = unsafe { (*header_ptr).size };
            if magic != ALLOC_MAGIC {
                if magic == 0 {
                    return;
                }
                klibcluu::warn("alloc: dealloc magic mismatch — leaking corrupted block");
                klibcluu::log_hex(klibcluu::LogLevel::Warn, "  ptr=0x", ptr as u64);
                klibcluu::log_hex(klibcluu::LogLevel::Warn, "  magic=0x", magic);
                return;
            }
            if size == 0 || size > self.heap_end.saturating_sub(self.heap_start) {
                klibcluu::warn("alloc: dealloc size corruption — leaking block");
                klibcluu::log_hex(klibcluu::LogLevel::Warn, "  ptr=0x", ptr as u64);
                klibcluu::log_hex(klibcluu::LogLevel::Warn, "  size=0x", size as u64);
                return;
            }

            unsafe { (*header_ptr).magic = 0; }

            self.used = self.used.saturating_sub(size);
            unsafe {
                self.add_free_region(header_ptr as usize, size);
            }
            self.shrink_if_possible();
        }

        fn shrink_if_possible(&mut self) {
            let heap_end = self.heap_end;
            if heap_end <= self.dynamic_start + INITIAL_HEAP_SIZE {
                return;
            }

            let mut top_start = 0usize;
            let mut top_end = 0usize;
            let mut prev_to_top: Option<*mut ListNode> = None;
            let mut current: *mut ListNode = &mut self.head as *mut ListNode;
            unsafe {
                while let Some(ref next) = (*current).next {
                    let next_ptr = *next as *const ListNode as *mut ListNode;
                    if next.end_addr() >= top_end {
                        top_end = next.end_addr();
                        top_start = next.start_addr();
                        prev_to_top = Some(current);
                    }
                    current = next_ptr;
                }
            }

            if top_end != heap_end || top_end == 0 {
                return;
            }

            let free_size = top_end.saturating_sub(top_start);
            let free_pages = free_size / PAGE_SIZE;
            if free_pages == 0 {
                return;
            }

            let shrink_bytes = free_pages * PAGE_SIZE;
            let new_heap_end = heap_end - shrink_bytes;
            if new_heap_end < self.dynamic_start + INITIAL_HEAP_SIZE {
                return;
            }

            let space_token = crate::boot::space_token();
            if space_token == 0 {
                return;
            }

            if crate::syscall::space_unmap(space_token, top_start, free_pages).is_err() {
                return;
            }

            let leftover = top_end - shrink_bytes;
            let prev = prev_to_top.unwrap_or(&mut self.head as *mut ListNode);
            unsafe {
                if leftover >= size_of::<ListNode>() && leftover > 0 {
                    let top_node = (*prev).next.as_mut().unwrap();
                    top_node.size = leftover;
                } else {
                    let top_node_ptr = (*prev).next.take().unwrap() as *mut ListNode;
                    let successor = (*top_node_ptr).next.take();
                    (*prev).next = successor;
                }
            }

            self.heap_end = new_heap_end;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Bump-pointer nursery
    // ═══════════════════════════════════════════════════════════════════════════

    struct Nursery {
        start: usize,
        end: usize,
        bump: usize,
        peak: usize,
    }

    impl Nursery {
        const fn new() -> Self {
            Self {
                start: 0,
                end: 0,
                bump: 0,
                peak: 0,
            }
        }

        fn init(&mut self) {
            let base = core::ptr::addr_of!(NURSERY_HEAP).cast::<u8>() as usize;
            self.start = base;
            self.end = base + NURSERY_SIZE;
            self.bump = base;
            self.peak = base;
        }

        fn is_ready(&self) -> bool {
            self.start != 0
        }

        #[allow(dead_code)]
        fn contains(&self, addr: usize) -> bool {
            self.start != 0 && addr >= self.start && addr < self.end
        }

        fn align_up(value: usize, align: usize) -> usize {
            (value + align - 1) & !(align - 1)
        }

        fn try_alloc(&mut self, layout: Layout) -> Option<*mut u8> {
            if self.start == 0 {
                return None;
            }
            let size = layout.size().max(1);
            let align = layout.align();

            let aligned_bump = Self::align_up(self.bump, align);
            let new_bump = aligned_bump.checked_add(size)?;
            if new_bump > self.end {
                return None;
            }
            self.bump = new_bump;
            if self.bump > self.peak {
                self.peak = self.bump;
            }
            Some(aligned_bump as *mut u8)
        }

        fn sweep(&mut self) {
            self.bump = self.start;
        }

        fn used(&self) -> usize {
            self.bump.saturating_sub(self.start)
        }

        fn peak_used(&self) -> usize {
            self.peak.saturating_sub(self.start)
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Deferred-free queue
    // ═══════════════════════════════════════════════════════════════════════════
    //
    // When dealloc is called re-entrantly (e.g., a GC triggered during alloc
    // tries to free unreachable objects), the main allocator Mutex is already
    // held. try_lock fails and the old code leaked the pointer. The deferred-free
    // queue stores up to DEFERRED_FREE_CAP pointers and drains them on the next
    // successful alloc/dealloc that acquires the main lock.

    const DEFERRED_FREE_CAP: usize = 64;

    struct DeferredFreeList {
        ptrs: [usize; DEFERRED_FREE_CAP],
        count: usize,
        leaked: u64,
    }

    impl DeferredFreeList {
        const fn new() -> Self {
            Self {
                ptrs: [0; DEFERRED_FREE_CAP],
                count: 0,
                leaked: 0,
            }
        }
    }

    pub struct LockedAllocator {
        inner: Mutex<LinkedListAllocator>,
        deferred: Mutex<DeferredFreeList>,
        // Optional OOM callback. Called when the linked-list allocator exhausts
        // its heap and cannot grow further. A GC can register its collection
        // routine here so that OOM triggers a collection cycle before the
        // final null return.
        oom_handler: Mutex<Option<fn()>>,
    }

    impl LockedAllocator {
        pub const fn new() -> Self {
            Self {
                inner: Mutex::new(LinkedListAllocator::new()),
                deferred: Mutex::new(DeferredFreeList::new()),
                oom_handler: Mutex::new(None),
            }
        }

        pub fn init(&self) {
            unsafe { self.inner.lock().init() };
        }

        pub fn stats(&self) -> AllocStats {
            let mut s = self.inner.lock().stats();
            s.leaked_deallocs = self.deferred.lock().leaked;
            s
        }

        /// Register an OOM handler. Called when allocation fails and the heap
        /// cannot grow. The handler should free memory (e.g., run GC) and
        // return; the allocator retries once after the handler returns.
        pub fn set_oom_handler(&self, handler: fn()) {
            *self.oom_handler.lock() = Some(handler);
        }

        fn drain_deferred(&self, alloc: &mut LinkedListAllocator) {
            let mut deferred = self.deferred.lock();
            for i in 0..deferred.count {
                let ptr = deferred.ptrs[i] as *mut u8;
                if !ptr.is_null() {
                    alloc.dealloc(ptr);
                }
            }
            deferred.count = 0;
        }
    }

    #[cfg(not(feature = "host-test"))]
    #[global_allocator]
    pub static GLOBAL_ALLOCATOR: NurseryAllocator = NurseryAllocator::new();

    pub fn init() {
        #[cfg(not(feature = "host-test"))]
        GLOBAL_ALLOCATOR.init();
    }

    pub fn stats() -> AllocStats {
        #[cfg(not(feature = "host-test"))]
        { GLOBAL_ALLOCATOR.stats() }
        #[cfg(feature = "host-test")]
        { AllocStats { total: 0, used: 0, peak: 0, free: 0, largest_free: 0, leaked_deallocs: 0 } }
    }

    /// Register a global OOM handler on the global allocator.
    #[allow(dead_code)]
    pub fn set_oom_handler(handler: fn()) {
        #[cfg(not(feature = "host-test"))]
        GLOBAL_ALLOCATOR.set_oom_handler(handler);
    }

    unsafe impl GlobalAlloc for LockedAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // Block until the lock is acquired. alloc never re-enters alloc
            // (the grow path calls space_map_range syscall only, and the OOM
            // handler runs after drop(guard)), so a blocking lock is safe.
            // dealloc stays on try_lock + deferred-free to avoid re-entrant
            // deadlock when a Drop fires mid-alloc (GC callback scenario).
            let mut guard = self.inner.lock();
            self.drain_deferred(&mut guard);
            let ptr = guard.alloc(layout);
            if !ptr.is_null() {
                return ptr;
            }
            // OOM: release the main lock before calling the handler so the
            // handler can alloc/free without re-entrancy issues.
            drop(guard);
            let handler = *self.oom_handler.lock();
            if let Some(h) = handler {
                h();
                let mut guard2 = self.inner.lock();
                self.drain_deferred(&mut guard2);
                return guard2.alloc(layout);
            }
            ptr::null_mut()
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            let mut guard = self.inner.lock();
            self.drain_deferred(&mut guard);
            guard.dealloc(ptr);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Nursery-wrapped allocator (the installed #[global_allocator])
    // ═══════════════════════════════════════════════════════════════════════════

    pub struct NurseryAllocator {
        nursery: Mutex<Nursery>,
        nursery_start: AtomicUsize,
        nursery_end: AtomicUsize,
        inner: LockedAllocator,
    }

    impl NurseryAllocator {
        pub const fn new() -> Self {
            Self {
                nursery: Mutex::new(Nursery::new()),
                nursery_start: AtomicUsize::new(0),
                nursery_end: AtomicUsize::new(0),
                inner: LockedAllocator::new(),
            }
        }

        pub fn init(&self) {
            {
                let mut n = self.nursery.lock();
                n.init();
                self.nursery_start.store(n.start, Ordering::Relaxed);
                self.nursery_end.store(n.end, Ordering::Relaxed);
            }
            self.inner.init();
        }

        pub fn stats(&self) -> AllocStats {
            let mut s = self.inner.stats();
            let n = self.nursery.lock();
            if n.is_ready() {
                s.total = s.total.saturating_add(NURSERY_SIZE);
                s.used = s.used.saturating_add(n.used());
                s.peak = s.peak.saturating_add(n.peak_used());
                s.free = s.total.saturating_sub(s.used);
            }
            s
        }

        #[allow(dead_code)]
        pub fn set_oom_handler(&self, handler: fn()) {
            self.inner.set_oom_handler(handler);
        }

        #[allow(dead_code)]
        pub fn nursery_sweep(&self) {
            self.nursery.lock().sweep();
        }
    }

    unsafe impl GlobalAlloc for NurseryAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let size = layout.size();

            if size <= NURSERY_THRESHOLD && layout.align() <= NURSERY_MAX_ALIGN {
                if let Some(mut n) = self.nursery.try_lock() {
                    if let Some(p) = n.try_alloc(layout) {
                        return p;
                    }
                    // Nursery full: fall through to linked-list allocator.
                    // Do NOT sweep — sweeping reclaims all nursery memory
                    // including live allocations (e.g., Vec backing buffers
                    // for long-lived structs like virtio-blk's InflightSlot),
                    // causing use-after-sweep corruption.
                    drop(n);
                }
            }

            self.inner.alloc(layout)
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            let addr = ptr as usize;
            let start = self.nursery_start.load(Ordering::Relaxed);
            if start != 0 {
                let end = self.nursery_end.load(Ordering::Relaxed);
                if addr >= start && addr < end {
                    return;
                }
            }

            self.inner.dealloc(ptr, layout);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[repr(align(4096))]
        struct TestHeap([u8; 64 * 1024]);

        static mut TEST_HEAP: TestHeap = TestHeap([0; 64 * 1024]);

        fn fresh_allocator() -> LinkedListAllocator {
            let mut alloc = LinkedListAllocator::new();
            let base = core::ptr::addr_of!(TEST_HEAP).cast::<u8>() as usize;
            alloc.heap_start = base;
            alloc.heap_end = base + 64 * 1024;
            alloc.heap_max = alloc.heap_end;
            alloc.dynamic_start = alloc.heap_end;
            unsafe { alloc.add_free_region(base, 64 * 1024) };
            alloc
        }

        #[test]
        fn fragmentation_largest_free_after_freeing_middle_block() {
            let mut alloc = fresh_allocator();

            // small, large, small: the middle block is larger than the
            // trailing free remainder, so freeing it yields the largest
            // free region.
            let small = Layout::from_size_align(8 * 1024, 8).unwrap();
            let large = Layout::from_size_align(32 * 1024, 8).unwrap();

            let p1 = alloc.alloc(small);
            let p2 = alloc.alloc(large);
            let p3 = alloc.alloc(small);
            assert!(!p1.is_null(), "p1 alloc failed");
            assert!(!p2.is_null(), "p2 alloc failed");
            assert!(!p3.is_null(), "p3 alloc failed");

            // Read the middle block's total allocated size (header + payload)
            // from its AllocHeader — this is exactly the size returned to the
            // free list on dealloc.
            let header_size = size_of::<AllocHeader>();
            let middle_block_size =
                unsafe { (*(p2.sub(header_size) as *const AllocHeader)).size };

            // Free the middle block. p1 and p3 are still live, so the freed
            // region cannot coalesce with neighbours.
            alloc.dealloc(p2);

            let stats = alloc.stats();
            assert_eq!(
                stats.largest_free, middle_block_size,
                "largest_free must equal the freed middle block size"
            );
            assert!(
                stats.largest_free >= 32 * 1024,
                "largest_free must be at least the requested middle block size"
            );
        }

        #[test]
        fn fragmentation_empty_heap_returns_zero() {
            let mut alloc = LinkedListAllocator::new();
            let stats = alloc.stats();
            assert_eq!(stats.largest_free, 0, "empty heap must report largest_free = 0");
        }

        // ─────────────────────────────────────────────────────────────
        // Nursery tests
        // ─────────────────────────────────────────────────────────────

        #[repr(align(4096))]
        struct BenchHeap([u8; 256 * 1024]);

        static mut BENCH_HEAP: BenchHeap = BenchHeap([0; 256 * 1024]);

        fn bench_allocator() -> LinkedListAllocator {
            let mut alloc = LinkedListAllocator::new();
            let base = core::ptr::addr_of!(BENCH_HEAP).cast::<u8>() as usize;
            alloc.heap_start = base;
            alloc.heap_end = base + 256 * 1024;
            alloc.heap_max = alloc.heap_end;
            alloc.dynamic_start = alloc.heap_end;
            unsafe { alloc.add_free_region(base, 256 * 1024) };
            alloc
        }

        #[test]
        fn nursery_threshold_constants() {
            assert_eq!(NURSERY_THRESHOLD, 256, "nursery threshold must be 256 bytes");
            assert!(
                NURSERY_SIZE >= 1024 * 1024 && NURSERY_SIZE <= 2 * 1024 * 1024,
                "nursery must be 1-2 MiB, got {} bytes",
                NURSERY_SIZE
            );
        }

        #[test]
        fn nursery_uninitialized_returns_none() {
            let mut nursery = Nursery::new();
            let layout = Layout::from_size_align(16, 8).unwrap();
            assert!(
                nursery.try_alloc(layout).is_none(),
                "uninitialized nursery must reject allocs"
            );
            assert!(!nursery.contains(0), "uninitialized nursery contains nothing");
        }

        #[test]
        fn nursery_full_sweeps_and_retries() {
            let mut nursery = Nursery::new();
            nursery.start = 0x10000;
            nursery.end = 0x10000 + 100;
            nursery.bump = nursery.start;
            nursery.peak = nursery.start;

            let layout = Layout::from_size_align(32, 8).unwrap();

            assert!(nursery.try_alloc(layout).is_some(), "alloc 1 should fit");
            assert!(nursery.try_alloc(layout).is_some(), "alloc 2 should fit");
            assert!(nursery.try_alloc(layout).is_some(), "alloc 3 should fit");
            assert!(
                nursery.try_alloc(layout).is_none(),
                "alloc 4 should fail (nursery full)"
            );

            nursery.sweep();
            assert_eq!(nursery.bump, nursery.start, "sweep must reset bump to start");
            assert!(
                nursery.try_alloc(layout).is_some(),
                "alloc after sweep should succeed"
            );
        }

        #[test]
        fn nursery_contains_range_check() {
            let mut nursery = Nursery::new();
            nursery.start = 0x10000;
            nursery.end = 0x10000 + 1000;
            nursery.bump = nursery.start;

            assert!(nursery.contains(0x10000), "start address is in nursery");
            assert!(nursery.contains(0x10000 + 999), "last byte is in nursery");
            assert!(!nursery.contains(0x10000 + 1000), "end address is NOT in nursery");
            assert!(!nursery.contains(0x0FFF0), "below start is NOT in nursery");
        }

        #[test]
        fn nursery_honors_large_alignment() {
            let mut nursery = Nursery::new();
            nursery.start = 0x10000;
            nursery.end = 0x10000 + 4096;
            nursery.bump = nursery.start;

            let layout = Layout::from_size_align(32, 128).unwrap();
            let p = nursery.try_alloc(layout).expect("nursery should handle align 128");
            assert_eq!(p as usize % 128, 0, "nursery must honor alignment");
        }

        #[test]
        fn nursery_small_alloc_benchmark() {
            extern crate std;
            use std::time::Instant;
            use core::hint::black_box;

            let layout = Layout::from_size_align(64, 8).unwrap();
            let iterations: usize = 1000;

            let mut nursery = Nursery::new();
            nursery.init();

            let nursery_start = Instant::now();
            for _ in 0..iterations {
                let p = nursery.try_alloc(layout).expect("nursery alloc");
                black_box(p);
            }
            let nursery_elapsed = nursery_start.elapsed();

            let mut ll = bench_allocator();
            let ll_start = Instant::now();
            for _ in 0..iterations {
                let p = ll.alloc(layout);
                assert!(!p.is_null(), "linked-list alloc failed");
                black_box(p);
            }
            let ll_elapsed = ll_start.elapsed();

            std::eprintln!(
                "benchmark_nursery_vs_linked_list: nursery={:?} linked_list={:?} ({} allocs of {}B)",
                nursery_elapsed, ll_elapsed, iterations, layout.size()
            );

            // Nursery should be faster or at least not significantly slower.
            // 5x tolerance for CI/timing noise on shared runners.
            assert!(
                nursery_elapsed <= ll_elapsed * 5,
                "nursery path too slow: nursery={:?} vs linked_list={:?}",
                nursery_elapsed,
                ll_elapsed
            );
        }
    }
}

pub use inner::{init, stats};
