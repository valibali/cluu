/*
 * Kernel Heap Allocator
 *
 * This module provides dynamic memory allocation for the kernel using a heap.
 * It builds on top of the linked_list_allocator crate which provides a simple
 * but functional heap implementation suitable for kernel use.
 *
 * DESIGN OVERVIEW:
 * - Fixed-size heap region in virtual memory (1 MiB by default)
 * - Heap is mapped to physical frames using the paging system
 * - Thread-safe allocation via LockedHeap (uses spin mutex internally)
 * - Supports standard Rust allocation APIs (Box, Vec, etc.)
 *
 * MEMORY LAYOUT:
 * - Heap virtual address: 0xffff_ffff_c000_0000 (high canonical address)
 * - Size: 1 MiB (configurable via HEAP_SIZE constant)
 * - Backing: Physical frames allocated via the frame allocator
 *
 * INITIALIZATION SEQUENCE:
 * 1. Map virtual heap range to physical frames
 * 2. Initialize the linked list allocator over the mapped region
 * 3. Register as global allocator for Rust's allocation APIs
 *
 * ERROR HANDLING:
 * - Allocation failures trigger kernel panic (alloc_error_handler)
 * - This is appropriate for kernel code where OOM is typically fatal
 */

use crate::memory::paging;
use linked_list_allocator::LockedHeap;
use x86_64::{VirtAddr, structures::paging::PageTableFlags};

/// Virtual address where the kernel heap begins
/// Uses high canonical address space to avoid conflicts with user space
pub const HEAP_START: u64 = 0xffff_ffff_c000_0000;

/// Size of the kernel heap in bytes (8 MiB)
/// Increased from 1 MiB to support more concurrent threads
/// Each thread needs 64KB stack, so 8 MiB supports ~128 threads
/// plus other kernel data structures
pub const HEAP_SIZE: u64 = 8 * 1024 * 1024; // 8 MiB

/// Global allocator instance used by Rust's allocation APIs
/// The #[global_allocator] attribute makes this the default allocator
/// for Box, Vec, HashMap, and other heap-allocated types
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Initialize the kernel heap
/// 
/// This function sets up the kernel's dynamic memory allocation system by:
/// 1. Mapping the heap virtual address range to physical memory
/// 2. Initializing the heap allocator over the mapped region
/// 
/// # Returns
/// * `Ok(())` - Heap successfully initialized
/// * `Err(&str)` - Initialization failed (usually due to mapping failure)
/// 
/// # Safety
/// This function must be called exactly once during kernel initialization,
/// after the physical frame allocator and paging system are set up.
pub fn init() -> Result<(), &'static str> {
    log::info!("Initializing kernel heap...");
    log::info!(
        "Heap range: 0x{:x} - 0x{:x} ({} KiB)",
        HEAP_START,
        HEAP_START + HEAP_SIZE - 1,
        HEAP_SIZE / 1024
    );

    // Convert heap start address to VirtAddr type for paging API
    let heap_start = VirtAddr::new(HEAP_START);
    
    // Set page flags: present in memory and writable (heap needs both)
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    // Map the entire heap virtual range to physical frames
    // This allocates physical memory and sets up page table entries
    paging::map_range(heap_start, HEAP_SIZE, flags)?;

    // Initialize the linked list allocator over the mapped memory region
    // SAFETY: We just mapped this range, so it's valid for the allocator to use
    unsafe {
        ALLOCATOR
            .lock()
            .init(HEAP_START as *mut u8, HEAP_SIZE as usize);
    }

    log::info!("Kernel heap initialized successfully");
    Ok(())
}

/// Allocation error handler (required when using a global allocator in no_std)
/// 
/// This function is called when heap allocation fails. In kernel context,
/// allocation failure is typically a fatal error since there's no user space
/// to return an error to, and the kernel needs its allocations to succeed.
/// 
/// # Arguments
/// * `layout` - Description of the allocation that failed (size, alignment)
/// 
/// # Behavior
/// Triggers a kernel panic with details about the failed allocation.
/// This will halt the system and display debugging information.
#[alloc_error_handler]
fn alloc_error(layout: core::alloc::Layout) -> ! {
    panic!("Kernel heap allocation failed: {:?}", layout);
}
