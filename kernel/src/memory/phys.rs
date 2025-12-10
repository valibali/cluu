/*
 * Physical Frame Allocator
 *
 * Bitmap-based allocator for 4 KiB frames.
 * Uses the BOOTBOOT memory map (embedded after the BOOTBOOT header).
 */

use crate::bootboot::{BOOTBOOT, BOOTBOOT_CORE, MMAP_FREE, MMapEnt};
use crate::memory::PhysFrame;
use spin::Mutex;

/// Maximum number of frames we can manage (1 GiB / 4 KiB = 262,144 frames)
const MAX_FRAMES: usize = 262_144;
const BITMAP_LEN: usize = MAX_FRAMES / 64;

/// Frame bitmap - each bit represents one 4 KiB frame
/// 0 = free, 1 = used
///
/// IMPORTANT: we never take & or &mut to this static; we only touch it
/// via raw pointers obtained from `addr_of!` / `addr_of_mut!`.
static mut FRAME_BITMAP: [u64; BITMAP_LEN] = [0; BITMAP_LEN];

/// Protects access to the frame bitmap
static ALLOCATOR_LOCK: Mutex<()> = Mutex::new(());

/// Kernel physical base address (where BOOTBOOT loads the kernel)
const KERNEL_PHYS_BASE: u64 = 0x0020_0000; // 2 MiB

/// Initialize the physical frame allocator from BOOTBOOT memory map
pub fn init_from_bootboot(bootboot_ptr: *const BOOTBOOT) {
    let _lock = ALLOCATOR_LOCK.lock();

    log::info!("Initializing physical frame allocator...");

    // Initially mark all frames as used.
    unsafe {
        // Get a raw pointer to the first u64 in FRAME_BITMAP without creating a reference
        let ptr = core::ptr::addr_of_mut!(FRAME_BITMAP) as *mut u64;
        for i in 0..BITMAP_LEN {
            // SAFETY: ptr points into our static array, i < BITMAP_LEN
            *ptr.add(i) = u64::MAX;
        }
    }

    let bootboot_ref = unsafe { &*bootboot_ptr };

    // Copy packed field to local (avoid unaligned reference)
    let bb_size = bootboot_ref.size;

    // num_entries = (bootboot.size - 128) / sizeof(MMapEnt) (16 bytes)
    let total_bytes = (bb_size as usize).saturating_sub(128);
    let mmap_entries = total_bytes / core::mem::size_of::<MMapEnt>();

    log::info!(
        "BOOTBOOT: size = {}, memory map entries = {}",
        bb_size,
        mmap_entries
    );

    // First entry is at bootboot.mmap, the rest are contiguous.
    let mmap_base: *const MMapEnt = core::ptr::addr_of!(bootboot_ref.mmap);

    for i in 0..mmap_entries {
        let entry = unsafe { &*mmap_base.add(i) };

        // Copy packed fields to locals to avoid unaligned references
        let region_ptr: u64 = entry.ptr;
        let raw_size: u64 = entry.size; // lower 4 bits store type
        let entry_type: u32 = (raw_size & 0xF) as u32;
        let region_size: u64 = raw_size & !0xF;

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

        if entry_type == MMAP_FREE {
            let start_frame = region_ptr / PhysFrame::SIZE;
            let end_frame = (region_ptr + region_size - 1) / PhysFrame::SIZE;

            log::info!("  Free region frames: {} - {}", start_frame, end_frame);

            for frame_num in start_frame..=end_frame {
                if (frame_num as usize) < MAX_FRAMES {
                    mark_frame_free(frame_num as usize);
                }
            }
        }
    }

    // Mark kernel frames as used
    mark_kernel_frames_used();

    log::info!("Physical frame allocator initialized");
}

/// Mark kernel frames as used based on linker symbols
fn mark_kernel_frames_used() {
    unsafe extern "C" {
        static __text_start: u8;
        static __bss_end: u8;
    }

    let kernel_virt_start = core::ptr::addr_of!(__text_start) as u64;
    let kernel_virt_end = core::ptr::addr_of!(__bss_end) as u64;

    // Convert to physical addresses:
    // Kernel linked at BOOTBOOT_CORE (virtual), loaded at 2 MiB (physical).
    let kernel_phys_start = kernel_virt_start - (BOOTBOOT_CORE as u64) + KERNEL_PHYS_BASE;
    let kernel_phys_end = kernel_virt_end - (BOOTBOOT_CORE as u64) + KERNEL_PHYS_BASE;

    let start_frame = kernel_phys_start / PhysFrame::SIZE;
    let end_frame = (kernel_phys_end + PhysFrame::SIZE - 1) / PhysFrame::SIZE;

    log::info!(
        "Marking kernel frames as used: phys 0x{:x}-0x{:x} (frames {}-{})",
        kernel_phys_start,
        kernel_phys_end,
        start_frame,
        end_frame
    );

    for frame_num in start_frame..end_frame {
        if (frame_num as usize) < MAX_FRAMES {
            mark_frame_used(frame_num as usize);
        }
    }
}

/// Allocate a physical frame
pub fn alloc_frame() -> Option<PhysFrame> {
    let _lock = ALLOCATOR_LOCK.lock();

    unsafe {
        let ptr = core::ptr::addr_of_mut!(FRAME_BITMAP) as *mut u64;

        for word_idx in 0..BITMAP_LEN {
            // Read current word value
            let word_val = *ptr.add(word_idx);
            if word_val != u64::MAX {
                // Found a word with at least one free bit
                for bit_idx in 0..64 {
                    let mask = 1u64 << bit_idx;
                    if (word_val & mask) == 0 {
                        // Found a free frame – set bit in bitmap
                        let new_word = word_val | mask;
                        *ptr.add(word_idx) = new_word;

                        let frame_num = word_idx * 64 + bit_idx;
                        if frame_num >= MAX_FRAMES {
                            return None;
                        }
                        let frame_addr = (frame_num as u64) * PhysFrame::SIZE;
                        return Some(PhysFrame::containing_address(frame_addr));
                    }
                }
            }
        }
    }

    None
}

/// Free a physical frame
pub fn free_frame(frame: PhysFrame) {
    let _lock = ALLOCATOR_LOCK.lock();

    let frame_num = (frame.start_address() / PhysFrame::SIZE) as usize;
    if frame_num < MAX_FRAMES {
        mark_frame_free(frame_num);
    }
}

/// Mark a frame as free in the bitmap
fn mark_frame_free(frame_num: usize) {
    let word_idx = frame_num / 64;
    let bit_idx = frame_num % 64;
    let mask = 1u64 << bit_idx;

    unsafe {
        let base = core::ptr::addr_of_mut!(FRAME_BITMAP) as *mut u64;
        let ptr = base.add(word_idx);
        let val = *ptr;
        *ptr = val & !mask;
    }
}

/// Mark a frame as used in the bitmap
fn mark_frame_used(frame_num: usize) {
    let word_idx = frame_num / 64;
    let bit_idx = frame_num % 64;
    let mask = 1u64 << bit_idx;

    unsafe {
        let base = core::ptr::addr_of_mut!(FRAME_BITMAP) as *mut u64;
        let ptr = base.add(word_idx);
        let val = *ptr;
        *ptr = val | mask;
    }
}

/// Get statistics about frame usage
pub fn get_stats() -> (usize, usize) {
    let _lock = ALLOCATOR_LOCK.lock();

    let mut used_frames = 0;
    let total_frames = MAX_FRAMES;

    unsafe {
        let base = core::ptr::addr_of!(FRAME_BITMAP) as *const u64;
        for i in 0..BITMAP_LEN {
            let word = *base.add(i);
            used_frames += word.count_ones() as usize;
        }
    }

    (used_frames, total_frames)
}
