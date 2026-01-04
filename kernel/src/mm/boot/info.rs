//! Boot Information Abstraction
//!
//! This module defines the abstract interface for boot information providers.
//! Different bootloaders (BOOTBOOT, Multiboot2, UEFI, Limine) can implement
//! this trait to provide memory information to the kernel.
//!
//! # Design Principles
//!
//! - **Dependency Inversion**: mm module depends on abstraction, not concrete bootloader
//! - **Open/Closed**: Extend by adding new adapters, don't modify mm module
//! - **Single Responsibility**: Each adapter handles one bootloader protocol

/// Memory region descriptor from bootloader
#[derive(Debug, Clone, Copy)]
pub struct BootMemoryRegion {
    /// Base physical address
    pub base: u64,
    /// Size in bytes
    pub size: u64,
    /// Region type
    pub region_type: MemoryRegionType,
}

/// Type of memory region
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRegionType {
    /// Usable RAM
    Usable,
    /// Reserved (firmware, ACPI tables, etc.)
    Reserved,
    /// ACPI reclaimable
    AcpiReclaimable,
    /// ACPI NVS
    AcpiNvs,
    /// Bad memory
    BadMemory,
    /// MMIO region
    Mmio,
}

/// Abstract boot information provider
///
/// Different bootloaders implement this trait to provide
/// boot-time information to the kernel.
pub trait BootInfoProvider {
    /// Get maximum physical address in the system
    ///
    /// Returns the highest physical address + 1 (exclusive end).
    fn max_physical_address(&self) -> u64;

    /// Get kernel physical address range
    ///
    /// Returns (start, end) where end is exclusive.
    fn kernel_physical_range(&self) -> (u64, u64);

    /// Get initrd location if present
    ///
    /// Returns Some((address, size)) or None
    fn initrd_location(&self) -> Option<(u64, u64)>;

    /// Get framebuffer information if present
    ///
    /// Returns Some((address, size)) or None
    fn framebuffer_location(&self) -> Option<(u64, u64)>;

    /// Get bootloader-specific info structure physical address
    ///
    /// Returns the physical address where the bootloader placed its info structure.
    fn info_structure_location(&self) -> u64;
}
