//! Memory Management Module
//!
//! This module implements the memory management subsystem for the CLUU microkernel,
//! following SOLID principles and clean architecture patterns.
//!
//! # Architecture
//!
//! The memory management system is divided into several components:
//!
//! - **Traits** (`traits`): Core interfaces following dependency inversion principle
//! - **Physical Memory Manager** (`pmm`): Buddy allocator for physical frames
//! - **Virtual Memory Manager** (`vmm`): Page table management using x86_64 crate
//! - **Address Space Management** (`space`): Virtual address space management
//! - **Page Fault Handler** (`fault`): Page fault handling with lazy allocation
//! - **Mock Implementations** (`mock`): Testing utilities
//!
//! # Design Principles
//!
//! ## Single Responsibility
//! Each module has one clear purpose:
//! - PMM manages physical memory allocation
//! - Traits define interfaces
//! - Mocks provide test doubles
//!
//! ## Open/Closed
//! The system is open for extension via trait implementation, closed for modification.
//! New allocation strategies can be added by implementing `PageAllocator`.
//!
//! ## Liskov Substitution
//! All trait implementations are interchangeable. Any `PageAllocator` can be used
//! wherever the trait is expected.
//!
//! ## Interface Segregation
//! Traits are small and focused. Components only depend on what they actually use.
//!
//! ## Dependency Inversion
//! High-level modules depend on abstractions (traits), not concrete implementations.
//!
//! # Example Usage
//!
//! ```rust,no_run
//! use crate::mm::{any PageAllocator implementation, MemoryRegion, PageAllocator};
//!
//! // Create allocator with memory regions
//! let regions = [MemoryRegion::new(0x100000, 0x100000)];
//! let mut allocator = any PageAllocator implementation::new(&regions);
//!
//! // Allocate a single page (order 0)
//! if let Some(addr) = allocator.alloc(0) {
//!     println!("Allocated page at {:?}", addr);
//!
//!     // Use the memory...
//!
//!     // Free it when done
//!     allocator.free(addr, 0);
//! }
//!
//! // Check statistics
//! let stats = allocator.stats();
//! println!("Free: {} / {} pages", stats.free_pages, stats.total_pages);
//! ```

// Core traits
pub mod traits;

// Bootloader abstraction layer
pub mod boot;

// Bootstrap bump allocator (used for heap allocations in other subsystems)
pub mod bump;

// Physical memory manager (bitmap allocator)
pub mod pmm;

// Physical memory direct mapping
pub mod physmap;

// Virtual memory manager (Page table operations)
pub mod vmm;

// Kernel heap allocator (dynamic memory for Vec, BTreeMap, etc.)
pub mod heap;

// Address space management
pub mod space;

// Page fault handler
pub mod fault;

pub mod space_repository;

// Capability-based frame accounting
pub mod frame_registry;

// Per-frame typed ownership table (Phase 1 — advisory)
pub mod frame_table;

// Userspace physical memory mapping
pub mod user_map;

// Page Attribute Table programming (write-combining, UC-, etc.)
pub mod pat;

// Mock implementations for testing
pub mod mock;

// Re-export key types for convenience
pub use traits::{
    AddressSpaceManager, AllocationStats, CreateSpaceError, DestroySpaceError, MapError,
    PageAllocator, PageFaultError, PageFaultErrorCode, PageFaultHandler, PageFlags, UnmapError,
    VirtualMemoryMapper,
};

pub use fault::FaultHandler;
pub use mock::MockPageAllocator;
pub use space::{layout, AddressSpace, HeapRegion, MemoryRegion};
pub use user_map::{map_phys_to_userspace, unmap_phys_from_userspace};
pub use vmm::PageTableManager;

/// PageAllocator adapter for the real PMM bitmap allocator.
///
/// Used when a `PageAllocator` trait object is needed for `PageTableManager`.
pub struct PmmPageAllocator;

impl PageAllocator for PmmPageAllocator {
    fn alloc(&mut self, order: usize) -> Option<x86_64::PhysAddr> {
        pmm::alloc_order(order).map(x86_64::PhysAddr::new)
    }

    fn free(&mut self, addr: x86_64::PhysAddr, order: usize) {
        pmm::free_order(addr.as_u64(), order);
    }

    fn stats(&self) -> AllocationStats {
        let (used, total) = pmm::get_stats();
        AllocationStats {
            total_pages: total,
            free_pages: total.saturating_sub(used),
            largest_free_order: 9,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// High-Level Memory Management Initialization
// ═══════════════════════════════════════════════════════════════════════════

/// Initialize memory management subsystem
///
/// This is the high-level orchestration function that:
/// 1. Gets boot information (bootloader-agnostic)
/// 2. Initializes physmap
/// 3. Creates new page tables with physmap
/// 4. Switches CR3 to new page tables
/// 5. Activates physmap
///
/// After this function returns, the kernel uses its own page tables
/// and bootloader mappings are no longer available.
///
/// # Arguments
///
/// * `boot_info` - Boot information provider (any bootloader)
///
/// # Safety
///
/// Must be called exactly once during boot, after hardware initialization
/// but before any heap allocation.
pub unsafe fn init(boot_info: &dyn boot::BootInfoProvider) {
    klibcluu::info("========================================");
    klibcluu::info("Memory Management Initialization");
    klibcluu::info("========================================");

    // Step 1: Get boot information (bootloader-agnostic)
    klibcluu::info("Step 1: Querying boot information...");
    let max_phys = boot_info.max_physical_address();
    let (kernel_phys_start, kernel_phys_end) = boot_info.kernel_physical_range();

    klibcluu::log_hex(
        klibcluu::LogLevel::Trace,
        "  Max physical address: 0x",
        max_phys,
    );
    klibcluu::log_hex(
        klibcluu::LogLevel::Trace,
        "  Kernel range: 0x",
        kernel_phys_start,
    );
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "    to 0x", kernel_phys_end);

    // Step 2: Initialize physmap module
    klibcluu::info("Step 2: Initializing physmap...");
    unsafe {
        physmap::init(max_phys);
    }

    // Step 3: Create new page tables with physmap
    klibcluu::info("Step 3: Creating page tables with physmap...");

    let pml4_phys = unsafe {
        vmm::create_initial_page_tables(boot_info, max_phys, kernel_phys_start, kernel_phys_end)
    };

    klibcluu::log_hex(
        klibcluu::LogLevel::Trace,
        "  PML4 at: 0x",
        pml4_phys.as_u64(),
    );

    // Step 4: Switch CR3 to new page tables
    klibcluu::info("Step 4: Switching to new page tables...");
    unsafe {
        vmm::switch_to_page_tables(pml4_phys);
    }

    // Step 5: Activate physmap
    klibcluu::info("Step 5: Activating physmap...");
    unsafe {
        physmap::activate();
    }

    // Step 6: Program PAT MSR (write-combining at index 1)
    klibcluu::info("Step 6: Programming PAT MSR...");
    pat::init();
    let pat_actual = pat::read();
    let pat_expected = pat::expected();
    if pat_actual == pat_expected {
        klibcluu::info("[BOOT] PAT programmed: ok");
    } else {
        klibcluu::info("[BOOT] PAT programmed: MISMATCH");
    }

    // Step 7: Build buddy free lists (requires physmap)
    klibcluu::info("Step 7: Building buddy allocator free lists...");
    pmm::init_buddy();

    klibcluu::info("========================================");
    klibcluu::info("Memory Management Ready:");
    klibcluu::info("  [✓] Own page tables active");
    klibcluu::info("  [✓] Physmap accessible");
    klibcluu::info("  [✓] Bootloader mappings discarded");
    klibcluu::info("========================================");
}

// ═══════════════════════════════════════════════════════════════════════════
// User Memory Allocation
// ═══════════════════════════════════════════════════════════════════════════

/// Allocate and map a userspace stack
///
/// Allocates physical frames and maps them into the user address space
/// for use as a stack. The stack grows downward from `stack_top`.
///
/// # Arguments
///
/// * `space` - Address space to map stack into
/// * `stack_top` - Top of stack (highest address, must be page-aligned)
/// * `stack_size_bytes` - Size of stack in bytes (will be rounded up to pages)
///
/// # Returns
///
/// Ok(()) on success, Err on allocation or mapping failure
///
/// # Example
///
/// ```rust,no_run
/// let stack_top = 0x7ff00000;  // 16MB below 2GB
/// let stack_size = 64 * 1024;  // 64KB stack
/// allocate_user_stack(&mut address_space, stack_top, stack_size)?;
/// ```
pub fn allocate_user_stack(
    space: &mut AddressSpace,
    stack_top: u64,
    stack_size_bytes: usize,
) -> Result<(), crate::error::Error> {
    use core::ptr::write_bytes;
    use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;

    // Round up to page boundary
    let stack_pages = stack_size_bytes.div_ceil(PAGE_SIZE);
    let page_table_root = space.page_table_root;

    klibcluu::trace("MM: Allocating user stack: ");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", stack_pages as u64);
    klibcluu::trace(" pages at 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", stack_top);

    // Allocate and map each stack page (from bottom to top)
    for i in 0..stack_pages {
        // Calculate virtual address (grow downward from top)
        let offset = ((stack_pages - i) as u64) * PAGE_SIZE as u64;
        let virt_addr = stack_top - offset;

        // Allocate physical frame
        let phys_frame = pmm::alloc_frame().ok_or(crate::error::Error::OutOfMemory)?;

        // Map page (writable, non-executable, user-accessible)
        unsafe {
            crate::elf::map_user_page(virt_addr, phys_frame, true, false, page_table_root, crate::token::scope::AddressSpaceId::new(0))
                .map_err(|_| crate::error::Error::OutOfMemory)?;
        }

        // Zero the page via physmap
        let phys_virt = unsafe { physmap::phys_to_virt_u64(phys_frame) };
        unsafe {
            write_bytes(phys_virt as *mut u8, 0, PAGE_SIZE);
        }
    }

    klibcluu::trace("MM: Stack mapped: 0x");
    klibcluu::log_hex(
        klibcluu::LogLevel::Trace,
        "",
        stack_top - (stack_pages as u64 * PAGE_SIZE as u64),
    );
    klibcluu::trace(" - 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", stack_top);

    Ok(())
}
