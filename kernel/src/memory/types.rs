/*
 * Memory Types
 *
 * This module defines core memory types used throughout the memory subsystem.
 * We re-export x86_64 crate types where appropriate and provide our own wrappers
 * for cleaner abstractions.
 */

// Re-export x86_64 types for convenience
pub use x86_64::{PhysAddr, VirtAddr};
pub use x86_64::structures::paging::{
    PageTableFlags, PhysFrame as X86PhysFrame,
};

/// Physical frame representation (4 KiB)
///
/// Represents a single 4 KiB aligned physical memory frame.
/// This is our internal type that wraps around addresses.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PhysFrame(u64);

impl PhysFrame {
    /// Size of a physical frame in bytes (4 KiB)
    pub const SIZE: u64 = 4096;

    /// Create a PhysFrame containing the given physical address
    /// Address is rounded down to 4 KiB boundary
    pub fn containing_address(addr: u64) -> Self {
        Self(addr & !0xfff)
    }

    /// Get the starting physical address of this frame
    pub fn start_address(&self) -> u64 {
        self.0
    }

    /// Get the ending physical address of this frame (inclusive)
    pub fn end_address(&self) -> u64 {
        self.0 + Self::SIZE - 1
    }

    /// Convert to x86_64 crate's PhysFrame type
    pub fn to_x86(&self) -> X86PhysFrame {
        X86PhysFrame::containing_address(PhysAddr::new(self.0))
    }

    /// Create from x86_64 crate's PhysFrame type
    pub fn from_x86(frame: X86PhysFrame) -> Self {
        Self(frame.start_address().as_u64())
    }
}

/// Page flags wrapper for cleaner API
#[derive(Copy, Clone, Debug)]
pub struct PageFlags(PageTableFlags);

impl PageFlags {
    /// Page is present in memory
    pub const PRESENT: Self = Self(PageTableFlags::PRESENT);
    /// Page is writable
    pub const WRITABLE: Self = Self(PageTableFlags::WRITABLE);
    /// Page is accessible from user mode
    pub const USER_ACCESSIBLE: Self = Self(PageTableFlags::USER_ACCESSIBLE);
    /// Disable execution on this page (requires NXE)
    pub const NO_EXECUTE: Self = Self(PageTableFlags::NO_EXECUTE);

    /// Create empty flags
    pub fn empty() -> Self {
        Self(PageTableFlags::empty())
    }

    /// Get the underlying PageTableFlags
    pub fn into_inner(self) -> PageTableFlags {
        self.0
    }

    /// Combine with another set of flags
    pub fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl core::ops::BitOr for PageFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl From<PageTableFlags> for PageFlags {
    fn from(flags: PageTableFlags) -> Self {
        Self(flags)
    }
}

impl From<PageFlags> for PageTableFlags {
    fn from(flags: PageFlags) -> Self {
        flags.0
    }
}
