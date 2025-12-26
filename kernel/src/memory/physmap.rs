/*
 * Physical Memory Direct Map (Physmap)
 *
 * This module implements the physmap - a direct mapping of all physical memory
 * into the kernel's virtual address space at a fixed high canonical address.
 *
 * WHY THIS IS IMPORTANT:
 * - Allows kernel to access any physical address without switching CR3
 * - Eliminates the "CR3 switching hack" needed to access other process page tables
 * - Provides clean abstraction for phys↔virt conversions
 * - Scales to arbitrary amounts of RAM (not limited by identity mapping)
 *
 * DESIGN:
 * - Physical [0..max_phys) is mapped to virtual [PHYS_MAP_BASE..PHYS_MAP_BASE+max_phys)
 * - PHYS_MAP_BASE is at 0xffff_8000_0000_0000 (start of higher half)
 * - Uses 4 KiB pages initially (2 MiB pages optional for performance later)
 * - Mapped with kernel-only permissions (not user-accessible)
 *
 * MEMORY LAYOUT:
 * ```
 * 0xffff_8000_0000_0000  ← PHYS_MAP_BASE (physmap start)
 * ...                     [Direct map of physical memory]
 * 0xffff_8000_0000_0000 + max_phys  ← physmap end
 * ...                     [unmapped]
 * 0xffff_ffff_f8000000   ← BOOTBOOT MMIO
 * 0xffff_ffff_fc000000   ← BOOTBOOT FB
 * 0xffff_ffff_c0000000   ← Kernel heap
 * 0xffff_ffff_ffe00000   ← BOOTBOOT info
 * 0xffff_ffff_ffe02000   ← Kernel code/data
 * ```
 */

use crate::memory::types::{PhysAddr, VirtAddr};

/// Base virtual address of the physmap (direct map)
///
/// This is at the start of the higher half canonical address space.
/// Chosen to not conflict with:
/// - BOOTBOOT structures (0xfffffffff8000000 and above)
/// - Kernel heap (0xffffffff_c0000000)
pub const PHYS_MAP_BASE: u64 = 0xffff_8000_0000_0000;

/// Maximum physical address that can be mapped
/// Initialized during boot from BOOTBOOT memory map
static mut MAX_PHYS_ADDR: u64 = 0;

/// Whether the physmap is actually mapped and usable
/// Set to true after CR3 switch to our own page tables
static mut PHYSMAP_ACTIVE: bool = false;

/// Initialize the physmap module
///
/// Must be called during boot after parsing BOOTBOOT memory map.
/// Sets the maximum physical address that will be mapped.
///
/// # Arguments
/// * `max_phys` - Highest physical address + 1 (exclusive end of physical memory)
///
/// # Safety
/// Must be called exactly once during boot, before any physmap operations.
pub unsafe fn init(max_phys: u64) { unsafe {
    MAX_PHYS_ADDR = max_phys;
    log::info!(
        "Physmap initialized: will map [0..0x{:x}) to [0x{:x}..0x{:x})",
        max_phys,
        PHYS_MAP_BASE,
        PHYS_MAP_BASE + max_phys
    );
}}

/// Get the maximum physical address
///
/// Returns the highest physical address + 1 (exclusive end).
pub fn max_phys() -> u64 {
    unsafe { MAX_PHYS_ADDR }
}

/// Mark the physmap as active (mapped and usable)
///
/// Call this after switching CR3 to page tables that include the physmap mapping.
///
/// # Safety
/// Must only be called after the physmap has been properly mapped in the active page tables.
pub unsafe fn activate() {
    unsafe {
        PHYSMAP_ACTIVE = true;
        log::debug!("Physmap activated - now using physmap for physical memory access");
    }
}

/// Check if the physmap is active and usable
///
/// Returns true if we've switched to our own page tables with physmap mapped.
/// Returns false during bootstrap when we must use BOOTBOOT's identity mapping.
#[inline]
pub fn is_active() -> bool {
    unsafe { PHYSMAP_ACTIVE }
}

/// Convert physical address to virtual address via physmap
///
/// # Arguments
/// * `phys` - Physical address to convert
///
/// # Returns
/// Virtual address in the physmap region
///
/// # Panics
/// Panics if physical address is beyond max_phys (indicates memory map parsing error)
#[inline]
pub fn phys_to_virt(phys: PhysAddr) -> VirtAddr {
    let phys_u64 = phys.as_u64();
    let max = unsafe { MAX_PHYS_ADDR };

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
/// # Arguments
/// * `virt` - Virtual address in physmap region
///
/// # Returns
/// * `Some(PhysAddr)` - If address is in physmap range
/// * `None` - If address is not in physmap range
#[inline]
pub fn virt_to_phys(virt: VirtAddr) -> Option<PhysAddr> {
    let virt_u64 = virt.as_u64();
    let max = unsafe { MAX_PHYS_ADDR };
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

/// Get a pointer to physical memory via physmap
///
/// This is the primary way to access physical memory after boot.
/// Returns a kernel virtual address that maps to the given physical address.
///
/// # Arguments
/// * `phys` - Physical address
///
/// # Returns
/// Mutable raw pointer to the memory
///
/// # Safety
/// The returned pointer is valid only if:
/// - The physmap has been properly set up
/// - The physical address is valid RAM or MMIO
/// - The caller ensures exclusive access if needed
#[inline]
pub unsafe fn phys_ptr<T>(phys: PhysAddr) -> *mut T {
    phys_to_virt(phys).as_mut_ptr()
}

/// Read a value from physical memory via physmap
///
/// # Safety
/// - Physical address must be valid and readable
/// - Type T must be valid for the data at that address
#[inline]
pub unsafe fn read_phys<T: Copy>(phys: PhysAddr) -> T { unsafe {
    let ptr: *const T = phys_ptr(phys);
    core::ptr::read_volatile(ptr)
}}

/// Write a value to physical memory via physmap
///
/// # Safety
/// - Physical address must be valid and writable
/// - Type T must be valid for the data at that address
#[inline]
pub unsafe fn write_phys<T: Copy>(phys: PhysAddr, value: T) { unsafe {
    let ptr: *mut T = phys_ptr(phys);
    core::ptr::write_volatile(ptr, value);
}}
