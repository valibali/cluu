//! Kernel Heap Allocator
//!
//! Provides dynamic memory allocation for kernel data structures (Vec, BTreeMap, etc.)
//! using a fixed-size heap region backed by physical frames.
//!
//! # Architecture
//!
//! - **Allocator**: linked_list_allocator (simple, proven implementation)
//! - **Backing**: Physical frames allocated via PMM
//! - **Mapping**: Virtual heap region mapped via VMM
//! - **Thread Safety**: LockedHeap uses spin mutex
//!
//! # Design Principles
//!
//! ## Single Responsibility
//! This module has one purpose: manage the kernel heap for dynamic allocations.
//! Page table management is delegated to VMM, physical allocation to PMM.
//!
//! ## Dependency Inversion
//! Depends on VMM abstraction for mapping, not concrete implementation.
//! The allocator doesn't care how pages are mapped, just that they are.
//!
//! ## Open/Closed
//! Can swap LockedHeap for a different allocator without changing the interface.
//!
//! # Memory Layout
//!
//! ```text
//! Virtual Address Space:
//! 0xffff_ffff_c000_0000 ┌─────────────────┐
//!                       │  Kernel Heap    │
//!                       │     (8 MiB)     │
//! 0xffff_ffff_c080_0000 └─────────────────┘
//! ```
//!
//! # Initialization Sequence
//!
//! 1. VMM maps heap virtual range to physical frames
//! 2. Initialize linked_list_allocator over mapped region
//! 3. Register as global allocator (already done at compile time)
//!
//! # Usage
//!
//! ```rust,no_run
//! // After heap is initialized, standard Rust types work:
//! let v = Vec::new();
//! let map = BTreeMap::new();
//! let s = String::from("hello");
//! ```

use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::LockedHeap;

/// Wrapper allocator that logs large allocation requests
///
/// This helps identify what's requesting large allocations (e.g., 4MB)
/// that might be causing heap exhaustion.
#[cfg_attr(any(test, feature = "testing"), allow(dead_code))]
struct LoggingAllocator;

unsafe impl GlobalAlloc for LoggingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Log large allocations (>1MB) to help diagnose issues
        const LARGE_ALLOC_THRESHOLD: usize = 1024 * 1024; // 1MB
        if layout.size() >= LARGE_ALLOC_THRESHOLD {
            let heap = INNER_ALLOCATOR.lock();
            klibcluu::warn("Large heap allocation requested");
            klibcluu::log_dec(klibcluu::LogLevel::Warn, "  Size: ", layout.size() as u64);
            klibcluu::log_dec(klibcluu::LogLevel::Warn, "  Align: ", layout.align() as u64);
            klibcluu::log_dec(
                klibcluu::LogLevel::Warn,
                "  Heap free: ",
                heap.free() as u64,
            );
            klibcluu::log_dec(
                klibcluu::LogLevel::Warn,
                "  Heap used: ",
                heap.used() as u64,
            );

            // Capture caller's RIP from stack frame (may not be reliable in all cases)
            // This is just for debugging - the real issue is why such large allocations are requested
            let caller_rip: u64;
            unsafe {
                core::arch::asm!(
                    "mov {}, [rbp + 8]",
                    out(reg) caller_rip,
                    options(nostack, nomem, preserves_flags)
                );
            }
            klibcluu::log_hex(klibcluu::LogLevel::Warn, "  Caller RIP: 0x", caller_rip);

            // Note: Progressive 1MB → 2MB → 4MB allocations suggest either:
            // 1. Multiple separate allocations
            // 2. Allocator trying different strategies
            // 3. Heap fragmentation preventing large contiguous blocks
            drop(heap);
        }

        // Delegate to inner allocator
        let result = INNER_ALLOCATOR.alloc(layout);

        // Log if allocation failed
        if result.is_null() && layout.size() >= LARGE_ALLOC_THRESHOLD {
            let heap = INNER_ALLOCATOR.lock();
            klibcluu::error("Large heap allocation FAILED");
            klibcluu::log_dec(
                klibcluu::LogLevel::Error,
                "  Requested: ",
                layout.size() as u64,
            );
            klibcluu::log_dec(
                klibcluu::LogLevel::Error,
                "  Heap free: ",
                heap.free() as u64,
            );
            klibcluu::log_dec(
                klibcluu::LogLevel::Error,
                "  Heap used: ",
                heap.used() as u64,
            );
            klibcluu::warn("  Likely cause: Heap fragmentation - free memory not contiguous");
        }

        result
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        INNER_ALLOCATOR.dealloc(ptr, layout)
    }
}

/// Virtual address where the kernel heap begins
///
/// Uses high canonical address space (negative addresses in x86-64)
/// to avoid conflicts with:
/// - User space: 0x0000_0000_0000_0000 - 0x0000_7fff_ffff_ffff
/// - Physmap:    0xffff_8000_0000_0000 - ...
/// - Kernel:     0xffff_ffff_ffe0_0000 - ...
pub const HEAP_START: u64 = 0xffff_ffff_c000_0000;

/// Size of the kernel heap in bytes (32 MiB)
///
/// Size rationale:
/// - Limited by BOOTBOOT's kernel virtual address space
/// - 32 MiB = 16 huge pages, still efficient mapping
/// - Increased from 16MB to accommodate token table growth and multi-process workloads
/// - Linux kernels typically use 32-128MB, this is reasonable for a microkernel
/// - Can be increased further when we implement our own address space management
pub const HEAP_SIZE: u64 = 32 * 1024 * 1024; // 32 MiB

/// Inner allocator (wrapped for logging)
static INNER_ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Global allocator instance with logging
///
/// The #[global_allocator] attribute makes this the default allocator
/// for all heap allocations (Box, Vec, String, etc.)
#[cfg(not(feature = "testing"))]
#[global_allocator]
static ALLOCATOR: LoggingAllocator = LoggingAllocator;

#[cfg(feature = "testing")]
#[allow(dead_code)]
static ALLOCATOR: LoggingAllocator = LoggingAllocator;

/// Initialize the kernel heap
///
/// Sets up dynamic memory allocation by:
/// 1. Mapping heap virtual range to physical frames (via VMM)
/// 2. Initializing the allocator over the mapped region
///
/// # Safety
///
/// Must be called exactly once during kernel initialization, after:
/// - PMM is initialized (provides physical frames)
/// - VMM is initialized (provides page table management)
/// - CR3 points to kernel page tables with physmap active
///
/// # Errors
///
/// Returns error if:
/// - Physical memory exhausted (can't allocate frames for heap)
/// - Page table mapping fails
///
/// # Example
///
/// ```rust,no_run
/// // In kernel initialization:
/// unsafe {
///     mm::heap::init()?;
/// }
///
/// // Now heap types work:
/// let v = vec![1, 2, 3];
/// ```
pub unsafe fn init() -> Result<(), &'static str> {
    klibcluu::info("Initializing kernel heap...");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "  Heap range: 0x", HEAP_START);
    klibcluu::log_hex(
        klibcluu::LogLevel::Trace,
        "    to 0x",
        HEAP_START + HEAP_SIZE - 1,
    );
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "  Size: ", HEAP_SIZE / 1024);
    klibcluu::trace(" KiB");

    // Map heap region to physical frames using VMM
    // This allocates frames from PMM and sets up page table entries
    unsafe {
        super::vmm::map_heap_region(HEAP_START, HEAP_SIZE)?;
    }

    // Initialize the linked list allocator over the mapped memory
    // SAFETY: We just mapped this range, so it's valid memory
    unsafe {
        INNER_ALLOCATOR
            .lock()
            .init(HEAP_START as *mut u8, HEAP_SIZE as usize);
    }

    klibcluu::info("Kernel heap initialized successfully");
    Ok(())
}

/// Allocation error handler
///
/// Required when using a global allocator in no_std environment.
/// Called when heap allocation fails (out of memory).
///
/// # Behavior
///
/// Panics with details about the failed allocation.
/// In kernel context, OOM is typically fatal since:
/// - No way to return error to "caller" (it's implicit via Box::new, etc.)
/// - Kernel needs its allocations to succeed for correct operation
///
/// Future improvement: Could try to reclaim memory or enter degraded mode
#[cfg(all(target_os = "none", not(feature = "testing")))]
#[alloc_error_handler]
fn alloc_error(layout: core::alloc::Layout) -> ! {
    let heap = INNER_ALLOCATOR.lock();
    klibcluu::error("Kernel heap allocation failed");
    klibcluu::log_dec(
        klibcluu::LogLevel::Error,
        "  Request size: ",
        layout.size() as u64,
    );
    klibcluu::log_dec(
        klibcluu::LogLevel::Error,
        "  Request align: ",
        layout.align() as u64,
    );
    klibcluu::log_dec(
        klibcluu::LogLevel::Error,
        "  Heap size: ",
        heap.size() as u64,
    );
    klibcluu::log_dec(
        klibcluu::LogLevel::Error,
        "  Heap used: ",
        heap.used() as u64,
    );
    klibcluu::log_dec(
        klibcluu::LogLevel::Error,
        "  Heap free: ",
        heap.free() as u64,
    );
    klibcluu::log_hex(
        klibcluu::LogLevel::Error,
        "  Heap bottom: 0x",
        heap.bottom() as u64,
    );
    klibcluu::log_hex(
        klibcluu::LogLevel::Error,
        "  Heap top: 0x",
        heap.top() as u64,
    );
    panic!("Kernel heap allocation failed: {:?}", layout);
}
