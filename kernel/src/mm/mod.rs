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

// Physical memory manager (Buddy allocator)
pub mod pmm;

// Virtual memory manager (Page table operations)
pub mod vmm;

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
