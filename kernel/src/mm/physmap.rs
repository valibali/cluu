//! Physical Memory Direct Mapping (Physmap)
//!
//! This module provides a direct mapping of all physical memory into kernel
//! virtual address space at a fixed high canonical address.
//!
//! # Why Physmap?
//!
//! - Access any physical address without CR3 switching
//! - Manipulate other processes' page tables directly
//! - Clean abstraction for physical ↔ virtual conversions
//! - Scales to arbitrary RAM sizes
//!
//! # Memory Layout
//!
//! ```text
//! 0xffff_8000_0000_0000  ← PHYS_MAP_BASE (physmap start)
//! ...                     [Direct map of all physical RAM]
//! 0xffff_8000_0000_0000 + max_phys  ← physmap end
//! ...                     [unmapped gap]
//! 0xffff_ffff_f8000000   ← BOOTBOOT MMIO
//! 0xffff_ffff_fc000000   ← BOOTBOOT Framebuffer
//! 0xffff_ffff_ffe00000   ← BOOTBOOT Info
//! 0xffff_ffff_ffe02000   ← Kernel Code/Data
//! ```

use x86_64::{PhysAddr, VirtAddr};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Base virtual address of the physmap
///
/// This is at the start of the higher half canonical address space,
/// chosen to not conflict with BOOTBOOT structures or kernel heap.
pub const PHYS_MAP_BASE: u64 = 0xffff_8000_0000_0000;

/// Maximum physical address that can be mapped
///
/// Initialized during boot from BOOTBOOT memory map.
static MAX_PHYS_ADDR: AtomicU64 = AtomicU64::new(0);

/// Whether the physmap is active and usable
///
/// Set to true after CR3 switch to our own page tables.
static PHYSMAP_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Initialize the physmap module
///
/// Sets the maximum physical address that will be mapped.
/// Must be called before creating page tables with physmap.
///
/// # Arguments
///
/// * `max_phys` - Highest physical address + 1 (exclusive end)
///
/// # Safety
///
/// Must be called exactly once during boot
pub unsafe fn init(max_phys: u64) {
    MAX_PHYS_ADDR.store(max_phys, Ordering::Relaxed);

    klibcluu::info("Physmap configuration:");
    klibcluu::log_hex(klibcluu::LogLevel::Info, "  Max physical: 0x", max_phys);
    klibcluu::log_hex(klibcluu::LogLevel::Info, "  Virtual base: 0x", PHYS_MAP_BASE);
    klibcluu::log_hex(
        klibcluu::LogLevel::Info,
        "  Virtual end:  0x",
        PHYS_MAP_BASE + max_phys
    );
}

/// Get the maximum physical address
///
/// Returns the highest physical address + 1 (exclusive end).
pub fn max_phys() -> u64 {
    MAX_PHYS_ADDR.load(Ordering::Relaxed)
}

/// Activate the physmap
///
/// Call this after switching CR3 to page tables that include physmap mapping.
///
/// # Safety
///
/// Must only be called after physmap is properly mapped in active page tables.
pub unsafe fn activate() {
    PHYSMAP_ACTIVE.store(true, Ordering::Release);
    klibcluu::info("Physmap activated - now using physmap for physical memory access");
}

/// Check if physmap is active
///
/// Returns true if we've switched to our page tables with physmap.
/// Returns false during bootstrap (using BOOTBOOT identity mapping).
#[inline]
pub fn is_active() -> bool {
    PHYSMAP_ACTIVE.load(Ordering::Acquire)
}

/// Convert physical address to virtual address via physmap
///
/// # Panics
///
/// Panics if physical address is beyond max_phys
#[inline]
pub fn phys_to_virt(phys: PhysAddr) -> VirtAddr {
    let phys_u64 = phys.as_u64();
    let max = max_phys();

    if phys_u64 >= max {
        panic!(
            "phys_to_virt: physical address 0x{:x} is beyond max_phys 0x{:x}",
            phys_u64, max
        );
    }

    VirtAddr::new(PHYS_MAP_BASE + phys_u64)
}

/// Convert virtual address in physmap to physical address
///
/// Returns Some(PhysAddr) if address is in physmap range, None otherwise.
#[inline]
pub fn virt_to_phys(virt: VirtAddr) -> Option<PhysAddr> {
    let virt_u64 = virt.as_u64();
    let max = max_phys();
    let end = PHYS_MAP_BASE + max;

    if virt_u64 >= PHYS_MAP_BASE && virt_u64 < end {
        Some(PhysAddr::new(virt_u64 - PHYS_MAP_BASE))
    } else {
        None
    }
}

/// Check if a virtual address is in the physmap region
#[inline]
pub fn is_physmap_addr(virt: VirtAddr) -> bool {
    virt_to_phys(virt).is_some()
}

/// Get a pointer to physical memory
///
/// During bootstrap (before physmap active): Uses BOOTBOOT identity mapping
/// After bootstrap: Uses physmap virtual address
///
/// This is the primary way to access physical memory safely.
///
/// # Safety
///
/// Caller must ensure:
/// - Physical address is valid RAM or MMIO
/// - Exclusive access if needed (no concurrent modifications)
#[inline]
pub unsafe fn phys_ptr<T>(phys: PhysAddr) -> *mut T {
    if is_active() {
        // Use physmap
        phys_to_virt(phys).as_mut_ptr()
    } else {
        // Bootstrap: BOOTBOOT identity maps all RAM
        phys.as_u64() as *mut T
    }
}

/// Convert physical address to virtual (returns u64 for convenience)
///
/// # Safety
///
/// Same as phys_ptr - physical address must be valid
#[inline]
pub unsafe fn phys_to_virt_u64(phys: u64) -> u64 {
    if is_active() {
        // Use physmap
        phys + PHYS_MAP_BASE
    } else {
        // Bootstrap: BOOTBOOT identity mapping
        phys
    }
}

/// Read a value from physical memory
///
/// # Safety
///
/// - Physical address must be valid and readable
/// - Type T must be valid for data at that address
#[inline]
pub unsafe fn read_phys<T: Copy>(phys: PhysAddr) -> T {
    let ptr: *const T = unsafe { phys_ptr(phys) };
    unsafe { core::ptr::read_volatile(ptr) }
}

/// Write a value to physical memory
///
/// # Safety
///
/// - Physical address must be valid and writable
/// - Type T must be valid for data at that address
#[inline]
pub unsafe fn write_phys<T: Copy>(phys: PhysAddr, value: T) {
    let ptr: *mut T = unsafe { phys_ptr(phys) };
    unsafe { core::ptr::write_volatile(ptr, value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phys_to_virt() {
        unsafe { init(0x100000000) }; // 4 GB

        let phys = PhysAddr::new(0x1000);
        let virt = phys_to_virt(phys);
        assert_eq!(virt.as_u64(), PHYS_MAP_BASE + 0x1000);
    }

    #[test]
    fn test_virt_to_phys() {
        unsafe { init(0x100000000) };

        let virt = VirtAddr::new(PHYS_MAP_BASE + 0x1000);
        let phys = virt_to_phys(virt).unwrap();
        assert_eq!(phys.as_u64(), 0x1000);
    }

    #[test]
    fn test_virt_to_phys_out_of_range() {
        unsafe { init(0x100000000) };

        // Address before physmap
        let virt = VirtAddr::new(0x1000);
        assert!(virt_to_phys(virt).is_none());

        // Address after physmap
        let virt = VirtAddr::new(PHYS_MAP_BASE + 0x200000000);
        assert!(virt_to_phys(virt).is_none());
    }
}
