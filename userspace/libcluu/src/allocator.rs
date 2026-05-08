//! Dynamic heap allocator for userspace processes.
//!
//! Two modes depending on feature flags:
//!
//! - **Pure Rust** (`!c-runtime`): linked-list allocator with dynamic heap growth
//! - **C runtime** (`c-runtime`): thin wrapper delegating to newlib's malloc/free

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

#[derive(Copy, Clone, Debug)]
pub struct AllocStats {
    pub total: usize,
    pub used: usize,
    pub peak: usize,
    pub free: usize,
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

    /// Maximum heap address for the Rust allocator.
    ///
    /// 8 MiB was too small for procmgr: a single failed `map_elf` would force
    /// the chunked-read fallback to allocate a contiguous `Vec<u8>` the size
    /// of the binary (4-5 MiB for /bin/ls etc.), and after the first such
    /// load the linked-list allocator's free regions were too fragmented for
    /// a second 4 MiB Vec to land. The slow-path allocation is wasteful in
    /// principle (the file is already in VFS's cache region), but until that
    /// path is replaced with a streaming ELF loader, give Rust binaries
    /// enough room to load the largest userspace ELF a couple of times.
    /// 56 MiB heap fits well below newlib's `_sbrk` range top (0x4000_0000)
    /// and the mmap region start (0x4100_0000).
    const USER_HEAP_MAX: usize = 0x0400_0000;

    /// Static bootstrap heap for early allocations before boot tokens are ready.
    #[repr(align(4096))]
    #[allow(dead_code)]
    struct StaticHeap([u8; STATIC_HEAP_SIZE]);

    static mut STATIC_HEAP: StaticHeap = StaticHeap([0; STATIC_HEAP_SIZE]);

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
            self.heap_max = USER_HEAP_MAX;
            self.dynamic_start = USER_HEAP_START;
            self.used = 0;
            self.peak = 0;
            self.head.next = None;

            let space_token = crate::boot::space_token();
            if space_token != 0 {
                let pages = INITIAL_HEAP_SIZE / PAGE_SIZE;
                if crate::syscall::space_map_range(space_token, USER_HEAP_START, 0, 0x03, pages, 0)
                    .is_ok()
                {
                    self.heap_start = USER_HEAP_START;
                    self.heap_end = USER_HEAP_START + INITIAL_HEAP_SIZE;
                    self.add_free_region(USER_HEAP_START, INITIAL_HEAP_SIZE);
                    return;
                }
            }

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

            let grow_size = min_size
                .max(double_size)
                .max(MIN_HEAP_GROW)
                .min(MAX_HEAP_GROW);

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

    #[cfg(not(feature = "host-test"))]
    #[global_allocator]
    pub static GLOBAL_ALLOCATOR: LockedAllocator = LockedAllocator::new();

    pub fn init() {
        #[cfg(not(feature = "host-test"))]
        GLOBAL_ALLOCATOR.init();
    }

    pub fn stats() -> AllocStats {
        #[cfg(not(feature = "host-test"))]
        { GLOBAL_ALLOCATOR.stats() }
        #[cfg(feature = "host-test")]
        { AllocStats { total: 0, used: 0, peak: 0, free: 0 } }
    }

    unsafe impl GlobalAlloc for LockedAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if let Some(mut guard) = self.inner.try_lock() {
                guard.alloc(layout)
            } else {
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
}

pub use inner::{init, stats};
