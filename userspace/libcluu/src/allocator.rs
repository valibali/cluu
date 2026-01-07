use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Number of bytes reserved for the runtime heap.
const HEAP_SIZE: usize = 128 * 1024;

/// Align the heap to 4 KiB boundaries.
#[repr(align(4096))]
struct HeapRegion([u8; HEAP_SIZE]);

// Heap must live in writable memory; use a mutable static to place it in .bss.
static mut HEAP: HeapRegion = HeapRegion([0; HEAP_SIZE]);

/// Simple bump allocator used by the userspace runtime.
pub struct BumpAllocator {
    next: AtomicUsize,
    end: AtomicUsize,
}

impl BumpAllocator {
    /// Creates an uninitialized allocator instance.
    pub const fn new() -> Self {
        Self {
            next: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
        }
    }

    /// Initializes the heap region. Must be called before any allocation.
    pub fn init(&self) {
        let start = unsafe { HEAP.0.as_ptr() as usize };
        let end = start + HEAP_SIZE;
        self.next.store(start, Ordering::SeqCst);
        self.end.store(end, Ordering::SeqCst);
    }

    #[inline]
    fn align_up(value: usize, align: usize) -> usize {
        (value + align - 1) & !(align - 1)
    }

    fn heap_end(&self) -> usize {
        self.end.load(Ordering::SeqCst)
    }
}

#[global_allocator]
pub static GLOBAL_ALLOCATOR: BumpAllocator = BumpAllocator::new();

pub fn init() {
    GLOBAL_ALLOCATOR.init();
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(1);
        let align = layout.align().max(core::mem::size_of::<usize>());

        loop {
            let current = self.next.load(Ordering::SeqCst);
            let start = Self::align_up(current, align);
            let next = match start.checked_add(size) {
                Some(next) => next,
                None => return ptr::null_mut(),
            };

            if next > self.heap_end() {
                return ptr::null_mut();
            }

            match self
                .next
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return start as *mut u8,
                Err(_) => continue,
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator never frees
    }
}
