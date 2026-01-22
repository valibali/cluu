use core::alloc::{GlobalAlloc, Layout};
use core::mem::{align_of, size_of};
use core::ptr;
use spin::Mutex;

/// Number of bytes reserved for the runtime heap.
const HEAP_SIZE: usize = 512 * 1024;

/// Align the heap to 4 KiB boundaries.
#[allow(dead_code)]
#[repr(align(4096))]
struct HeapRegion([u8; HEAP_SIZE]);

// Heap must live in writable memory; use a mutable static to place it in .bss.
static mut HEAP: HeapRegion = HeapRegion([0; HEAP_SIZE]);

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
    heap_end: usize,
    used: usize,
    peak: usize,
}

impl LinkedListAllocator {
    const fn new() -> Self {
        Self {
            head: ListNode::new(0),
            heap_start: 0,
            heap_end: 0,
            used: 0,
            peak: 0,
        }
    }

    unsafe fn init(&mut self) {
        let start = core::ptr::addr_of!(HEAP).cast::<u8>() as usize;
        let end = start + HEAP_SIZE;
        self.heap_start = start;
        self.heap_end = end;
        self.used = 0;
        self.peak = 0;
        self.head.next = None;
        self.add_free_region(start, HEAP_SIZE);
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
            if let Some(next_next) = next.next.as_mut() {
                if next.end_addr() == next_next.start_addr() {
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

    fn alloc(&mut self, layout: Layout) -> *mut u8 {
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

                return user_start as *mut u8;
            }

            current = current.next.as_mut().unwrap();
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
