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
//! use crate::mm::{BuddyAllocator, MemoryRegion, PageAllocator};
//!
//! // Create allocator with memory regions
//! let regions = [MemoryRegion::new(0x100000, 0x100000)];
//! let mut allocator = BuddyAllocator::new(&regions);
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

// Physical memory manager (Buddy allocator)
pub mod pmm;

// Simple physical memory manager (for early boot, used for page tables)
pub mod pmm_simple;

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

// Mock implementations for testing
pub mod mock;

// Re-export key types for convenience
pub use traits::{
    PageAllocator,
    AllocationStats,
    VirtualMemoryMapper,
    AddressSpaceManager,
    PageFaultHandler,
    PageFlags,
    PageFaultErrorCode,
    MapError,
    UnmapError,
    CreateSpaceError,
    DestroySpaceError,
    PageFaultError,
};

pub use pmm::{BuddyAllocator, MemoryRegion};
pub use vmm::PageTableManager;
pub use space::{AddressSpace, HeapRegion, MemoryRegion as SpaceMemoryRegion, layout};
pub use fault::FaultHandler;
pub use mock::MockPageAllocator;

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

    klibcluu::log_hex(klibcluu::LogLevel::Info, "  Max physical address: 0x", max_phys);
    klibcluu::log_hex(klibcluu::LogLevel::Info, "  Kernel range: 0x", kernel_phys_start);
    klibcluu::log_hex(klibcluu::LogLevel::Info, "    to 0x", kernel_phys_end);

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

    klibcluu::log_hex(klibcluu::LogLevel::Info, "  PML4 at: 0x", pml4_phys.as_u64());

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

    klibcluu::info("========================================");
    klibcluu::info("Memory Management Ready:");
    klibcluu::info("  [✓] Own page tables active");
    klibcluu::info("  [✓] Physmap accessible");
    klibcluu::info("  [✓] Bootloader mappings discarded");
    klibcluu::info("========================================");
}
