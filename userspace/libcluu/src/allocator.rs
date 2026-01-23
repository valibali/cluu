//! Dynamic heap allocator for userspace processes.
//!
//! This allocator eagerly maps a dynamic heap region at init time for a unified,
//! contiguous heap. A small static buffer is kept as fallback for very early
//! allocations before boot tokens are available.
//!
//! # Design
//!
//! - Primary heap: 256KB initial at USER_HEAP_START, grows dynamically
//! - Fallback heap: 64KB static buffer in .bss (bootstrap only)
//! - Growth strategy: Double current size (min 256KB, max 16MB per growth)
//! - Maximum heap: ~1GB (0x40000000 - 0x00800000)
//! - Allocation strategy: First-fit linked list with coalescing

use core::alloc::{GlobalAlloc, Layout};
use core::mem::{align_of, size_of};
use core::ptr;
use spin::Mutex;

/// Size of static bootstrap heap (64KB - fallback for early init).
const STATIC_HEAP_SIZE: usize = 64 * 1024;

/// Initial dynamic heap size (256KB - mapped eagerly at init).
const INITIAL_HEAP_SIZE: usize = 256 * 1024;

/// Minimum heap growth increment (256KB).
const MIN_HEAP_GROW: usize = 256 * 1024;

/// Maximum single heap growth (16MB - prevents excessive single allocations).
const MAX_HEAP_GROW: usize = 16 * 1024 * 1024;

/// Page size for heap allocation.
const PAGE_SIZE: usize = 4096;

/// Start of dynamic userspace heap region (must match kernel's USER_HEAP_START).
const USER_HEAP_START: usize = 0x0080_0000;

/// Maximum heap address (must match kernel's USER_HEAP_MAX).
const USER_HEAP_MAX: usize = 0x4000_0000;

/// Static bootstrap heap for early allocations before boot tokens are ready.
#[repr(align(4096))]
struct StaticHeap([u8; STATIC_HEAP_SIZE]);

static mut STATIC_HEAP: StaticHeap = StaticHeap([0; STATIC_HEAP_SIZE]);

#[derive(Copy, Clone, Debug)]
pub struct AllocStats {
    pub total: usize,
    pub used: usize,
    pub peak: usize,
    pub free: usize,
}

#[repr(C)]
struct AllocHeader {
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
    heap_end: usize,      // Current mapped end (for dynamic region)
    heap_max: usize,      // Maximum allowed address
    dynamic_start: usize, // Start of dynamic heap region
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

    /// Initialize the allocator.
    ///
    /// Tries to eagerly map a dynamic heap region for a unified, contiguous heap.
    /// Falls back to static heap if dynamic mapping fails (e.g., before boot tokens ready).
    unsafe fn init(&mut self) {
        self.heap_max = USER_HEAP_MAX;
        self.dynamic_start = USER_HEAP_START;
        self.used = 0;
        self.peak = 0;
        self.head.next = None;

        // Try to map initial dynamic heap immediately
        let space_token = crate::boot::space_token();
        if space_token != 0 {
            let pages = INITIAL_HEAP_SIZE / PAGE_SIZE;
            if crate::syscall::space_map_range(
                space_token,
                USER_HEAP_START,
                0,    // source_ptr (0 = zero-fill)
                0x03, // flags: read + write
                pages,
                0, // data_len
            )
            .is_ok()
            {
                // Success - use dynamic heap as primary
                self.heap_start = USER_HEAP_START;
                self.heap_end = USER_HEAP_START + INITIAL_HEAP_SIZE;
                self.add_free_region(USER_HEAP_START, INITIAL_HEAP_SIZE);
                return;
            }
        }

        // Fallback to static heap if dynamic init fails
        // (bootstrap case before boot tokens ready)
        let static_start = core::ptr::addr_of!(STATIC_HEAP).cast::<u8>() as usize;
        let static_end = static_start + STATIC_HEAP_SIZE;

        self.heap_start = static_start;
        self.heap_end = static_end;
        self.add_free_region(static_start, STATIC_HEAP_SIZE);
    }

    fn stats(&self) -> AllocStats {
        let total = self.heap_end.saturating_sub(self.heap_start);
        let used = self.used;
        let free = total.saturating_sub(used);
        AllocStats {
            total,
            used,
            peak: self.peak,
            free,
        }
    }

    fn align_up(value: usize, align: usize) -> usize {
        (value + align - 1) & !(align - 1)
    }

    /// Grow the heap by mapping additional pages via syscall.
    ///
    /// Uses a doubling strategy: tries to double current heap size (clamped to min/max).
    /// This reduces fragmentation and syscall overhead for large allocations.
    ///
    /// Returns true if growth succeeded, false otherwise.
    fn grow_heap(&mut self, min_size: usize) -> bool {
        // Get space token - if not available, can't grow dynamically
        let space_token = crate::boot::space_token();
        if space_token == 0 {
            return false;
        }

        // Calculate where to start the new mapping
        // First dynamic allocation starts at USER_HEAP_START
        // Subsequent ones continue from heap_end if it's in the dynamic region
        let map_start = if self.heap_end >= self.dynamic_start {
            self.heap_end
        } else {
            self.dynamic_start
        };

        // Calculate how much to grow using doubling strategy:
        // - At least min_size (what the allocation needs)
        // - At least MIN_HEAP_GROW (256KB minimum)
        // - Try to double current heap size (reduces future growth syscalls)
        // - At most MAX_HEAP_GROW (16MB cap to prevent excessive single mapping)
        let current_size = if self.heap_end >= self.dynamic_start {
            self.heap_end.saturating_sub(self.dynamic_start)
        } else {
            0
        };
        let double_size = current_size; // Double = add current size again

        let grow_size = min_size
            .max(double_size)
            .max(MIN_HEAP_GROW)
            .min(MAX_HEAP_GROW);

        let grow_pages = (grow_size + PAGE_SIZE - 1) / PAGE_SIZE;
        let actual_grow = grow_pages * PAGE_SIZE;
        let new_end = map_start.saturating_add(actual_grow);

        // Check if we'd exceed max
        if new_end > self.heap_max {
            // Try with just min_size if doubling would exceed
            let fallback_pages = (min_size + PAGE_SIZE - 1) / PAGE_SIZE;
            let fallback_grow = fallback_pages * PAGE_SIZE;
            let fallback_end = map_start.saturating_add(fallback_grow);

            if fallback_end > self.heap_max {
                return false;
            }

            // Use fallback size
            return self.do_grow(space_token, map_start, fallback_pages, fallback_grow);
        }

        self.do_grow(space_token, map_start, grow_pages, actual_grow)
    }

    /// Actually perform the heap growth mapping.
    fn do_grow(
        &mut self,
        space_token: usize,
        map_start: usize,
        pages: usize,
        size: usize,
    ) -> bool {
        // Request kernel to map pages (zero-filled, read+write)
        // flags: 0x03 = read (0x01) + write (0x02)
        match crate::syscall::space_map_range(
            space_token,
            map_start, // virt_start
            0,         // source_ptr (0 = zero-fill)
            0x03,      // flags: read + write
            pages,
            0, // data_len
        ) {
            Ok(_) => {
                // Add new region to free list
                unsafe {
                    self.add_free_region(map_start, size);
                }
                // Update heap_end to track the dynamic region
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

    /// Try to allocate without growing the heap.
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

    /// Allocate memory, growing the heap if necessary.
    fn alloc(&mut self, layout: Layout) -> *mut u8 {
        // Try allocation first
        if let Some(ptr) = self.try_alloc(layout) {
            return ptr;
        }

        // Allocation failed - try to grow heap dynamically
        // Request enough for the allocation plus some overhead and slack
        let needed = layout.size() + size_of::<AllocHeader>() + align_of::<ListNode>() + 64;
        if self.grow_heap(needed) {
            // Retry allocation after growth
            if let Some(ptr) = self.try_alloc(layout) {
                return ptr;
            }
        }

        // Growth failed or still can't allocate
        ptr::null_mut()
    }

    fn dealloc(&mut self, ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }
        let header_size = size_of::<AllocHeader>();
        let header_ptr = unsafe { ptr.sub(header_size) } as *mut AllocHeader;
        let size = unsafe { (*header_ptr).size };
        if size == 0 {
            return;
        }

        self.used = self.used.saturating_sub(size);
        unsafe {
            self.add_free_region(header_ptr as usize, size);
        }
    }
}

pub struct LockedAllocator {
    inner: Mutex<LinkedListAllocator>,
}

impl LockedAllocator {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(LinkedListAllocator::new()),
        }
    }

    pub fn init(&self) {
        unsafe { self.inner.lock().init() };
    }

    pub fn stats(&self) -> AllocStats {
        self.inner.lock().stats()
    }
}

#[global_allocator]
pub static GLOBAL_ALLOCATOR: LockedAllocator = LockedAllocator::new();

pub fn init() {
    GLOBAL_ALLOCATOR.init();
}

pub fn stats() -> AllocStats {
    GLOBAL_ALLOCATOR.stats()
}

unsafe impl GlobalAlloc for LockedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if let Some(mut guard) = self.inner.try_lock() {
            guard.alloc(layout)
        } else {
            // Avoid deadlock on re-entrant allocation; signal OOM to caller.
            ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if let Some(mut guard) = self.inner.try_lock() {
            guard.dealloc(ptr)
        } else {
            // Avoid deadlock on re-entrant free; leak as a safe fallback.
        }
    }
}
