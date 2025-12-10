/*
 * Memory Management
 *
 * This is the top-level memory management module that coordinates all aspects
 * of kernel memory management. It provides a unified interface and ensures
 * proper initialization order of the various memory subsystems.
 *
 * ARCHITECTURE OVERVIEW:
 *
 * 1. Physical Memory Management (phys module):
 *    - Manages 4 KiB physical memory frames
 *    - Uses bitmap-based allocation for simplicity and speed
 *    - Initialized from BOOTBOOT memory map
 *    - Provides alloc_frame() and free_frame() APIs
 *
 * 2. Virtual Memory Management (paging module):
 *    - Manages page table operations and virtual-to-physical mappings
 *    - Integrates with physical allocator for backing storage
 *    - Provides map_page(), unmap_page(), and range operations
 *    - Uses x86_64 crate for low-level page table manipulation
 *
 * 3. Kernel Heap (heap module):
 *    - Provides dynamic memory allocation (malloc/free equivalent)
 *    - Built on top of virtual memory system
 *    - Uses linked_list_allocator for heap management
 *    - Enables Rust's Box, Vec, HashMap, etc. in kernel
 *
 * INITIALIZATION SEQUENCE:
 * The order of initialization is critical:
 * 1. Physical frame allocator (needs BOOTBOOT memory map)
 * 2. Paging system (needs frame allocator for page tables)
 * 3. Kernel heap (needs paging system for virtual memory)
 *
 * MEMORY LAYOUT:
 * - Physical: Managed as 4 KiB frames starting from address 0
 * - Virtual: Kernel heap at 0xffff_ffff_c000_0000
 * - Page tables: Accessed via identity mapping (for now)
 *
 * THREAD SAFETY:
 * All subsystems use appropriate synchronization (spin mutexes)
 * to ensure safe concurrent access from multiple kernel threads.
 */

pub mod heap;
pub mod paging;
pub mod phys;

use crate::bootboot::BOOTBOOT;

/// Physical frame representation (4 KiB)
///
/// Represents a single 4 KiB aligned physical memory frame.
/// This is the fundamental unit of physical memory management.
///
/// The frame address is always aligned to 4 KiB boundaries (bottom 12 bits are 0).
/// This alignment is enforced by the containing_address constructor.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PhysFrame(u64);

impl PhysFrame {
    /// Size of a physical frame in bytes (4 KiB = 4096 bytes)
    /// This matches the x86_64 page size and is a hardware requirement
    pub const SIZE: u64 = 4096;

    /// Create a PhysFrame containing the given physical address
    ///
    /// The address will be rounded down to the nearest 4 KiB boundary.
    /// For example, addresses 0x1000-0x1FFF all belong to frame 0x1000.
    ///
    /// # Arguments
    /// * `addr` - Any physical address within the desired frame
    ///
    /// # Returns
    /// PhysFrame representing the 4 KiB frame containing the address
    pub fn containing_address(addr: u64) -> Self {
        // Clear bottom 12 bits to align to 4 KiB boundary
        Self(addr & !0xfff)
    }

    /// Get the starting physical address of this frame
    ///
    /// # Returns
    /// Physical address of the first byte in this frame (4 KiB aligned)
    pub fn start_address(&self) -> u64 {
        self.0
    }

    /// Get the ending physical address of this frame
    ///
    /// # Returns
    /// Physical address of the last byte in this frame (start + 4095)
    pub fn end_address(&self) -> u64 {
        self.0 + Self::SIZE - 1
    }
}

/// Top-level memory management initialization
///
/// Initializes all memory management subsystems in the correct order.
/// This function must be called exactly once during kernel boot, after
/// the BOOTBOOT structure is available but before any dynamic allocation.
///
/// # Arguments
/// * `bootboot_ptr` - Pointer to BOOTBOOT structure containing memory map
///
/// # Initialization Order
/// 1. Physical frame allocator - parses memory map, sets up bitmap
/// 2. Paging system - initializes page table mapper
/// 3. Kernel heap - maps heap region and initializes allocator
///
/// # Panics
/// Panics if heap initialization fails, as this is a fatal error
/// that prevents normal kernel operation.
///
/// # Safety
/// The caller must ensure bootboot_ptr points to a valid BOOTBOOT structure.
pub fn init(bootboot_ptr: *const BOOTBOOT) {
    log::info!("Initializing memory management...");

    // Step 1: Initialize physical frame allocator
    // This must come first as other subsystems depend on frame allocation
    phys::init_from_bootboot(bootboot_ptr);

    // Step 2: Initialize paging system
    // This must come after frame allocator (needs frames for page tables)
    // but before heap (heap needs virtual memory mapping)
    paging::init();

    // Step 3: Initialize kernel heap
    // This must come last as it depends on both frame allocation and paging
    heap::init().expect("Failed to initialize kernel heap");

    // Log memory usage statistics for debugging
    let (used, total) = phys::get_stats();
    log::info!(
        "Memory management initialized - Physical memory: {} used / {} total frames ({:.1}% used)",
        used,
        total,
        (used as f32 / total as f32) * 100.0
    );
}
