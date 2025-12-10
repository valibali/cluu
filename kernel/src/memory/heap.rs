/*
 * Kernel Heap Allocator
 *
 * Uses linked_list_allocator::LockedHeap on top of a mapped heap range.
 */

use crate::memory::paging;
use linked_list_allocator::LockedHeap;
use x86_64::{VirtAddr, structures::paging::PageTableFlags};

/// Heap virtual address range
pub const HEAP_START: u64 = 0xffff_ffff_c000_0000;
pub const HEAP_SIZE: u64 = 1024 * 1024; // 1 MiB

/// Global allocator instance
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Initialize the kernel heap
pub fn init() -> Result<(), &'static str> {
    log::info!("Initializing kernel heap...");
    log::info!(
        "Heap range: 0x{:x} - 0x{:x} ({} KiB)",
        HEAP_START,
        HEAP_START + HEAP_SIZE - 1,
        HEAP_SIZE / 1024
    );

    let heap_start = VirtAddr::new(HEAP_START);
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    // Map the heap region
    paging::map_range(heap_start, HEAP_SIZE, flags)?;

    // Initialize the allocator over that range
    unsafe {
        ALLOCATOR
            .lock()
            .init(HEAP_START as *mut u8, HEAP_SIZE as usize);
    }

    log::info!("Kernel heap initialized successfully");
    Ok(())
}

/// Allocation error handler (required when using a global allocator in no_std)
#[alloc_error_handler]
fn alloc_error(layout: core::alloc::Layout) -> ! {
    panic!("Allocation error: {:?}", layout);
}
