/*
 * Physical Frame Allocator
 *
 * This module implements a bitmap-based physical memory allocator for 4 KiB frames.
 * It uses the BOOTBOOT bootloader's memory map to determine available physical memory.
 *
 * DESIGN OVERVIEW:
 * - Each 4 KiB physical memory frame is represented by one bit in the bitmap
 * - 0 = frame is free and available for allocation
 * - 1 = frame is used/reserved and cannot be allocated
 * - Maximum manageable memory: 1 GiB (262,144 frames)
 * - Thread-safe access via spin mutex
 *
 * INITIALIZATION PROCESS:
 * 1. Mark all frames as used initially (conservative approach)
 * 2. Parse BOOTBOOT memory map entries
 * 3. Mark frames in free regions as available
 * 4. Reserve kernel frames based on linker symbols
 *
 * ALLOCATION STRATEGY:
 * - First-fit allocation: scan bitmap from start to find first free frame
 * - Atomic bit manipulation to mark frames as used/free
 * - No fragmentation handling (simple bitmap approach)
 *
 * MEMORY LAYOUT ASSUMPTIONS:
 * - Kernel loaded at 2 MiB physical address (BOOTBOOT standard)
 * - Frame size is fixed at 4 KiB (x86_64 page size)
 * - Physical memory starts at address 0x0
 */

use crate::bootboot::{BOOTBOOT, BOOTBOOT_CORE, MMAP_FREE, MMapEnt};
use crate::memory::PhysFrame;
use spin::Mutex;

// Import linker symbols that mark kernel boundaries
// These are defined in the linker script and mark start/end of kernel sections
unsafe extern "C" {
    static __text_start: u8; // Start of kernel text section
    static __bss_end: u8; // End of kernel BSS section (last kernel data)
}

/// Maximum number of frames we can manage (1 GiB / 4 KiB = 262,144 frames)
/// This limit keeps the bitmap size reasonable while supporting most systems
const MAX_FRAMES: usize = 262_144;

/// Number of u64 words needed for the bitmap (each u64 holds 64 frame bits)
const BITMAP_LEN: usize = MAX_FRAMES / 64;

/// Frame bitmap - each bit represents one 4 KiB frame
/// Bit value meanings: 0 = free, 1 = used/reserved
///
/// SAFETY NOTE: This static is accessed only via raw pointers to avoid
/// creating intermediate references that could cause undefined behavior
/// in concurrent scenarios. All access is protected by ALLOCATOR_LOCK.
static mut FRAME_BITMAP: [u64; BITMAP_LEN] = [0; BITMAP_LEN];

/// Mutex protecting concurrent access to the frame bitmap
/// Ensures atomic allocation/deallocation operations
static ALLOCATOR_LOCK: Mutex<()> = Mutex::new(());

/// Physical address where BOOTBOOT loads the kernel (standard location)
/// This is used to calculate which frames contain kernel code/data
const KERNEL_PHYS_BASE: u64 = 0x0020_0000; // 2 MiB

/// Initialize the physical frame allocator from BOOTBOOT memory map
///
/// This function parses the BOOTBOOT memory map to identify free physical
/// memory regions and initializes the frame bitmap accordingly.
///
/// # Arguments
/// * `bootboot_ptr` - Pointer to the BOOTBOOT structure containing memory map
///
/// # Safety
/// The caller must ensure bootboot_ptr points to a valid BOOTBOOT structure
pub fn init_from_bootboot(bootboot_ptr: *const BOOTBOOT) {
    // Acquire exclusive access to prevent concurrent initialization
    let _lock = ALLOCATOR_LOCK.lock();

    log::info!("Initializing physical frame allocator...");

    // Step 1: Conservative initialization - mark all frames as used
    // This prevents accidental allocation of unknown memory regions
    unsafe {
        // Use raw pointer arithmetic to avoid creating intermediate references
        let ptr = core::ptr::addr_of_mut!(FRAME_BITMAP) as *mut u64;
        for i in 0..BITMAP_LEN {
            // Set all bits to 1 (used state) - SAFETY: i is bounded by BITMAP_LEN
            *ptr.add(i) = u64::MAX;
        }
    }

    // Step 2: Parse BOOTBOOT structure to extract memory map information
    let bootboot_ref = unsafe { &*bootboot_ptr };

    // Copy packed field to local variable to avoid unaligned memory access
    // BOOTBOOT structure may not be properly aligned for direct field access
    let bb_size = bootboot_ref.size;

    // Calculate number of memory map entries
    // Memory map starts after 128-byte BOOTBOOT header, each entry is 16 bytes
    let total_bytes = (bb_size as usize).saturating_sub(128);
    let mmap_entries = total_bytes / core::mem::size_of::<MMapEnt>();

    log::info!(
        "BOOTBOOT: size = {}, memory map entries = {}",
        bb_size,
        mmap_entries
    );

    // Get pointer to first memory map entry (located after BOOTBOOT header)
    let mmap_base: *const MMapEnt = core::ptr::addr_of!(bootboot_ref.mmap);

    // Step 3: Process each memory map entry to identify free regions
    for i in 0..mmap_entries {
        let entry = unsafe { &*mmap_base.add(i) };

        // Extract fields from packed structure to avoid alignment issues
        let region_ptr: u64 = entry.ptr; // Physical start address
        let raw_size: u64 = entry.size; // Size with type in lower 4 bits
        let entry_type: u32 = (raw_size & 0xF) as u32; // Memory region type
        let region_size: u64 = raw_size & !0xF; // Actual size (clear type bits)

        // Skip zero-sized entries (shouldn't happen but be defensive)
        if region_size == 0 {
            continue;
        }

        log::info!(
            "MMAP entry {}: ptr=0x{:x}, size=0x{:x}, type={}",
            i,
            region_ptr,
            region_size,
            entry_type
        );

        // Only process free memory regions (MMAP_FREE = available for OS use)
        if entry_type == MMAP_FREE {
            // Convert physical address range to frame numbers
            let start_frame = region_ptr / PhysFrame::SIZE;
            let end_frame = (region_ptr + region_size - 1) / PhysFrame::SIZE;

            log::info!("  Free region frames: {} - {}", start_frame, end_frame);

            // Mark all frames in this region as available for allocation
            for frame_num in start_frame..=end_frame {
                // Bounds check to prevent bitmap overflow
                if (frame_num as usize) < MAX_FRAMES {
                    mark_frame_free(frame_num as usize);
                }
            }
        }
    }

    // Step 4: Reserve frames occupied by kernel code and data
    mark_kernel_frames_used();

    log::info!("Physical frame allocator initialized");
}

/// Mark kernel frames as used based on linker symbols
///
/// This function uses linker-provided symbols to determine the physical
/// memory range occupied by the kernel and marks those frames as reserved.
/// This prevents the allocator from giving out frames that contain kernel code.
fn mark_kernel_frames_used() {
    // Get virtual addresses of kernel boundaries from linker symbols
    let kernel_virt_start = core::ptr::addr_of!(__text_start) as u64;
    let kernel_virt_end = core::ptr::addr_of!(__bss_end) as u64;

    // Convert virtual addresses to physical addresses
    // Kernel is linked at BOOTBOOT_CORE virtual address but loaded at 2 MiB physical
    // Physical = Virtual - Link_Base + Load_Base
    let kernel_phys_start = kernel_virt_start - (BOOTBOOT_CORE as u64) + KERNEL_PHYS_BASE;
    let kernel_phys_end = kernel_virt_end - (BOOTBOOT_CORE as u64) + KERNEL_PHYS_BASE;

    // Convert physical address range to frame numbers
    let start_frame = kernel_phys_start / PhysFrame::SIZE;
    // Round up end address to include partial frames
    let end_frame = (kernel_phys_end + PhysFrame::SIZE - 1) / PhysFrame::SIZE;

    log::info!(
        "Marking kernel frames as used: phys 0x{:x}-0x{:x} (frames {}-{})",
        kernel_phys_start,
        kernel_phys_end,
        start_frame,
        end_frame
    );

    // Mark all kernel frames as used to prevent allocation
    for frame_num in start_frame..end_frame {
        // Bounds check to prevent bitmap overflow
        if (frame_num as usize) < MAX_FRAMES {
            mark_frame_used(frame_num as usize);
        }
    }
}

/// Allocate a physical frame using first-fit strategy
///
/// Scans the bitmap from the beginning to find the first available frame.
/// This is simple but can lead to fragmentation over time.
///
/// # Returns
/// * `Some(PhysFrame)` - Successfully allocated frame
/// * `None` - No free frames available (out of memory)
pub fn alloc_frame() -> Option<PhysFrame> {
    // Acquire lock to ensure atomic allocation
    let _lock = ALLOCATOR_LOCK.lock();

    unsafe {
        // Get raw pointer to bitmap for direct manipulation
        let ptr = core::ptr::addr_of_mut!(FRAME_BITMAP) as *mut u64;

        // Scan bitmap word by word (64 frames at a time)
        for word_idx in 0..BITMAP_LEN {
            // Read current 64-bit word from bitmap
            let word_val = *ptr.add(word_idx);

            // Skip words where all frames are used (all bits set)
            if word_val != u64::MAX {
                // Found a word with at least one free frame - scan individual bits
                for bit_idx in 0..64 {
                    let mask = 1u64 << bit_idx;

                    // Check if this frame is free (bit is 0)
                    if (word_val & mask) == 0 {
                        // Mark frame as used by setting the bit
                        let new_word = word_val | mask;
                        *ptr.add(word_idx) = new_word;

                        // Calculate frame number and validate bounds
                        let frame_num = word_idx * 64 + bit_idx;
                        if frame_num >= MAX_FRAMES {
                            return None; // Frame number exceeds our limit
                        }

                        // Convert frame number to physical address
                        let frame_addr = (frame_num as u64) * PhysFrame::SIZE;
                        return Some(PhysFrame::containing_address(frame_addr));
                    }
                }
            }
        }
    }

    // No free frames found
    None
}

/// Free a physical frame and return it to the available pool
///
/// # Arguments
/// * `frame` - The physical frame to free
///
/// # Safety
/// The caller must ensure the frame is no longer in use and contains
/// no important data, as it may be immediately reallocated.
pub fn free_frame(frame: PhysFrame) {
    // Acquire lock to ensure atomic deallocation
    let _lock = ALLOCATOR_LOCK.lock();

    // Convert physical address back to frame number
    let frame_num = (frame.start_address() / PhysFrame::SIZE) as usize;

    // Bounds check before modifying bitmap
    if frame_num < MAX_FRAMES {
        mark_frame_free(frame_num);
    }
}

/// Mark a specific frame as free in the bitmap
///
/// # Arguments
/// * `frame_num` - Frame number to mark as free (0-based index)
///
/// # Safety
/// Caller must ensure frame_num is within valid range and the frame
/// is safe to reuse (no longer contains important data).
fn mark_frame_free(frame_num: usize) {
    // Calculate which 64-bit word and which bit within that word
    let word_idx = frame_num / 64; // Which u64 in the array
    let bit_idx = frame_num % 64; // Which bit in that u64
    let mask = 1u64 << bit_idx; // Bitmask for this specific frame

    unsafe {
        // Get pointer to the specific word containing our frame's bit
        let base = core::ptr::addr_of_mut!(FRAME_BITMAP) as *mut u64;
        let ptr = base.add(word_idx);
        let val = *ptr;
        // Clear the bit (set to 0 = free) using bitwise AND with inverted mask
        *ptr = val & !mask;
    }
}

/// Mark a specific frame as used in the bitmap
///
/// # Arguments
/// * `frame_num` - Frame number to mark as used (0-based index)
fn mark_frame_used(frame_num: usize) {
    // Calculate bitmap position (same logic as mark_frame_free)
    let word_idx = frame_num / 64; // Which u64 in the array
    let bit_idx = frame_num % 64; // Which bit in that u64
    let mask = 1u64 << bit_idx; // Bitmask for this specific frame

    unsafe {
        // Get pointer to the specific word containing our frame's bit
        let base = core::ptr::addr_of_mut!(FRAME_BITMAP) as *mut u64;
        let ptr = base.add(word_idx);
        let val = *ptr;
        // Set the bit (set to 1 = used) using bitwise OR
        *ptr = val | mask;
    }
}

/// Get statistics about physical memory usage
///
/// # Returns
/// * `(used_frames, total_frames)` - Tuple containing used and total frame counts
///
/// This function scans the entire bitmap to count used frames, so it may
/// be expensive to call frequently.
pub fn get_stats() -> (usize, usize) {
    // Acquire lock to get consistent snapshot of bitmap state
    let _lock = ALLOCATOR_LOCK.lock();

    let mut used_frames = 0;
    let total_frames = MAX_FRAMES;

    unsafe {
        // Scan entire bitmap and count set bits (used frames)
        let base = core::ptr::addr_of!(FRAME_BITMAP) as *const u64;
        for i in 0..BITMAP_LEN {
            let word = *base.add(i);
            // Use hardware population count instruction for efficiency
            used_frames += word.count_ones() as usize;
        }
    }

    (used_frames, total_frames)
}
