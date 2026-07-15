//! Memory Management Traits
//!
//! This module defines the core traits for memory management following SOLID principles:
//! - Single Responsibility: Each trait has one focused purpose
//! - Open/Closed: Extend via trait implementations
//! - Liskov Substitution: All implementors are interchangeable
//! - Interface Segregation: Small, focused traits
//! - Dependency Inversion: Depend on abstractions, not concrete types

use x86_64::{PhysAddr, VirtAddr};

/// Physical page allocation strategy
///
/// This trait defines the interface for allocating and freeing physical memory pages.
/// Implementations might use different strategies (buddy, bitmap, slab, etc.)
///
/// # Thread Safety
/// Implementors must be Send + Sync as allocators may be accessed from multiple contexts.
pub trait PageAllocator: Send + Sync {
    /// Allocate 2^order contiguous pages
    ///
    /// # Arguments
    /// * `order` - Allocation order where size = 2^order pages
    ///
    /// # Returns
    /// * `Some(PhysAddr)` - Physical address of allocated region
    /// * `None` - If allocation failed (out of memory or fragmentation)
    ///
    /// # Alignment
    /// The returned address must be aligned to `2^order * PAGE_SIZE` bytes.
    fn alloc(&mut self, order: usize) -> Option<PhysAddr>;

    /// Free 2^order contiguous pages starting at addr
    ///
    /// # Arguments
    /// * `addr` - Physical address previously returned by `alloc`
    /// * `order` - Same order used in the allocation
    ///
    /// # Safety
    /// Caller must ensure:
    /// - `addr` was returned by a previous `alloc` call
    /// - `order` matches the order used in that allocation
    /// - The memory is not currently in use
    fn free(&mut self, addr: PhysAddr, order: usize);

    /// Get allocation statistics
    ///
    /// Returns current memory usage statistics for monitoring and debugging.
    fn stats(&self) -> AllocationStats;
}

/// Allocation statistics
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocationStats {
    /// Number of free pages available
    pub free_pages: usize,
    /// Total number of pages managed
    pub total_pages: usize,
    /// Largest contiguous free block (as order)
    pub largest_free_order: usize,
}

/// Virtual memory mapping operations
///
/// This trait defines operations for mapping virtual addresses to physical addresses
/// and managing page table entries.
pub trait VirtualMemoryMapper: Send {
    /// Map a virtual page to a physical frame
    ///
    /// # Arguments
    /// * `virt` - Virtual address to map (will be page-aligned)
    /// * `phys` - Physical address to map to
    /// * `flags` - Page table flags (present, writable, user, etc.)
    fn map(&mut self, virt: VirtAddr, phys: PhysAddr, flags: PageFlags) -> Result<(), MapError>;

    /// Unmap a virtual page
    ///
    /// # Returns
    /// * `Ok(PhysAddr)` - The physical address that was mapped
    /// * `Err` - If the page wasn't mapped
    fn unmap(&mut self, virt: VirtAddr) -> Result<PhysAddr, UnmapError>;

    /// Change page protection flags
    fn protect(&mut self, virt: VirtAddr, flags: PageFlags) -> Result<(), MapError>;

    /// Translate virtual address to physical
    ///
    /// # Returns
    /// * `Some(PhysAddr)` - The physical address if mapped
    /// * `None` - If not mapped
    fn translate(&self, virt: VirtAddr) -> Option<PhysAddr>;
}

/// Address space management
///
/// This trait defines operations for creating and managing per-process address spaces.
pub trait AddressSpaceManager: Send {
    /// Handle to an address space
    type SpaceHandle;

    /// Create a new address space
    fn create(&mut self) -> Result<Self::SpaceHandle, CreateSpaceError>;

    /// Destroy an address space
    fn destroy(&mut self, space: Self::SpaceHandle) -> Result<(), DestroySpaceError>;

    /// Switch to an address space
    fn switch_to(&mut self, space: &Self::SpaceHandle);

    /// Get current active address space
    fn current(&self) -> Option<&Self::SpaceHandle>;
}

// ═══════════════════════════════════════════════════════════════════════════
// Type Definitions
// ═══════════════════════════════════════════════════════════════════════════

/// Page table flags
///
/// These correspond to x86_64 page table entry flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags {
    /// Present bit - page is currently in physical memory
    pub present: bool,
    /// Writable bit - page can be written to
    pub writable: bool,
    /// User accessible bit - page accessible from ring 3
    pub user: bool,
    /// Write-through caching
    pub write_through: bool,
    /// Cache disabled
    pub cache_disabled: bool,
    /// Accessed bit - set by CPU when page is accessed
    pub accessed: bool,
    /// Dirty bit - set by CPU when page is written to
    pub dirty: bool,
    /// Huge page bit (2MB or 1GB page)
    pub huge: bool,
    /// Global bit - prevents TLB flush
    pub global: bool,
    /// No-execute bit - prevents code execution
    pub no_execute: bool,
}

impl PageFlags {
    /// Default kernel flags: present, writable, not user-accessible
    pub const fn kernel() -> Self {
        Self {
            present: true,
            writable: true,
            user: false,
            write_through: false,
            cache_disabled: false,
            accessed: false,
            dirty: false,
            huge: false,
            global: false,
            no_execute: false,
        }
    }

    /// Default user flags: present, writable, user-accessible
    pub const fn user() -> Self {
        Self {
            present: true,
            writable: true,
            user: true,
            write_through: false,
            cache_disabled: false,
            accessed: false,
            dirty: false,
            huge: false,
            global: false,
            no_execute: false,
        }
    }

    /// Read-only user flags
    pub const fn user_readonly() -> Self {
        Self {
            present: true,
            writable: false,
            user: true,
            write_through: false,
            cache_disabled: false,
            accessed: false,
            dirty: false,
            huge: false,
            global: false,
            no_execute: false,
        }
    }
}

/// Page fault error code
#[derive(Debug, Clone, Copy)]
pub struct PageFaultErrorCode {
    /// If set, fault caused by page-protection violation. Otherwise, non-present page.
    pub protection_violation: bool,
    /// If set, fault caused by write access. Otherwise, read access.
    pub write: bool,
    /// If set, fault occurred in user mode. Otherwise, kernel mode.
    pub user: bool,
    /// If set, fault caused by reserved bit violation.
    pub reserved_write: bool,
    /// If set, fault caused by instruction fetch.
    pub instruction_fetch: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
// Error Types
// ═══════════════════════════════════════════════════════════════════════════

/// Errors that can occur during mapping
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// Page is already mapped
    AlreadyMapped,
    /// Out of memory (couldn't allocate page table)
    OutOfMemory,
    /// Invalid address (e.g., not page-aligned)
    InvalidAddress,
}

/// Errors that can occur during unmapping
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmapError {
    /// Page is not mapped
    NotMapped,
    /// Invalid address
    InvalidAddress,
}

/// Errors that can occur during space creation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateSpaceError {
    /// Out of memory
    OutOfMemory,
    /// Too many address spaces
    TooManySpaces,
}

/// Errors that can occur during space destruction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestroySpaceError {
    /// Space handle is invalid
    InvalidSpace,
    /// Cannot destroy active space
    SpaceActive,
}

/// Errors that can occur during page fault handling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageFaultError {
    /// Fault is unrecoverable
    Unrecoverable,
    /// Out of memory when trying to handle fault
    OutOfMemory,
    /// Invalid access (e.g., write to read-only page)
    InvalidAccess,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_flags_default_kernel() {
        let flags = PageFlags::kernel();
        assert!(flags.present);
        assert!(flags.writable);
        assert!(!flags.user);
    }

    #[test]
    fn test_page_flags_default_user() {
        let flags = PageFlags::user();
        assert!(flags.present);
        assert!(flags.writable);
        assert!(flags.user);
    }

    #[test]
    fn test_page_flags_user_readonly() {
        let flags = PageFlags::user_readonly();
        assert!(flags.present);
        assert!(!flags.writable);
        assert!(flags.user);
    }

    #[test]
    fn test_allocation_stats() {
        let stats = AllocationStats {
            free_pages: 100,
            total_pages: 256,
            largest_free_order: 7,
        };
        assert_eq!(stats.free_pages, 100);
        assert_eq!(stats.total_pages, 256);
        assert_eq!(stats.largest_free_order, 7);
    }
}
