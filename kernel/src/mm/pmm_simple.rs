//! Simple Physical Memory Manager
//!
//! Manages physical frames using a bitmap allocator.
//! Returns physical addresses that can be accessed via BOOTBOOT's identity mapping.

use crate::bootboot::{BOOTBOOT, MMapEnt};
use crate::mm::boot::info::BootInfoProvider;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Maximum number of physical frames we can track
/// For 4GB of RAM with 4KB pages = 1M frames = 128KB bitmap
const MAX_FRAMES: usize = 1024 * 1024; // 1M frames = 4GB

/// Bitmap to track free/allocated frames
/// Each bit represents one 4KB frame
static mut FRAME_BITMAP: [u64; MAX_FRAMES / 64] = [0; MAX_FRAMES / 64];

/// Total number of frames available
static TOTAL_FRAMES: AtomicUsize = AtomicUsize::new(0);

/// Number of allocated frames
static USED_FRAMES: AtomicUsize = AtomicUsize::new(0);

/// Initialize the physical memory manager
///
/// Parses BOOTBOOT memory map and marks free regions in the bitmap
/// Reserves all system regions: low memory, kernel, initrd, BOOTBOOT, framebuffer
pub unsafe fn init(
    bootboot: &BOOTBOOT,
    boot_info: &dyn BootInfoProvider,
) {
    klibcluu::info("Initializing Physical Memory Manager...");

    // Get kernel range from boot info
    let (kernel_phys_start, kernel_phys_end) = boot_info.kernel_physical_range();

    // Clear bitmap (all frames marked as used initially)
    unsafe {
        for i in 0..FRAME_BITMAP.len() {
            FRAME_BITMAP[i] = 0;
        }
    }

    // Parse memory map and mark free regions
    let mmap_entries = bootboot.size as usize / 16;
    let mmap_ptr = &bootboot.mmap as *const MMapEnt;

    let mut total_free_frames = 0;

    for i in 0..mmap_entries {
        let entry = unsafe { &*mmap_ptr.add(i) };

        // Extract type from size field (lower 4 bits)
        let entry_type = entry.size & 0xF;
        let size = entry.size & !0xF;

        // Only mark usable memory as free (type 1 = MMAP_FREE)
        if entry_type == 1 {
            let start_addr = entry.ptr;
            let end_addr = start_addr + size;

            // Mark frames in this region as free
            // Never allocate frame 0 (reserved for BIOS/NULL pointer protection)
            let start_frame = ((start_addr / 4096) as usize).max(1);
            let end_frame = ((end_addr + 4095) / 4096) as usize;

            for frame in start_frame..end_frame.min(MAX_FRAMES) {
                mark_free(frame);
                total_free_frames += 1;
            }
        }
    }

    TOTAL_FRAMES.store(total_free_frames, Ordering::SeqCst);

    // ===== RESERVE SYSTEM REGIONS =====

    // 1. Reserve first 1 MB (NULL page, IVT, BIOS data area)
    // CRITICAL for NULL pointer safety and legacy compatibility
    const LOW_MEMORY_RESERVE: u64 = 0x100000; // 1 MB
    let low_mem_start_frame = 0;
    let low_mem_end_frame = (LOW_MEMORY_RESERVE / 4096) as usize;
    for frame in low_mem_start_frame..low_mem_end_frame {
        mark_used(frame);
    }
    klibcluu::log_hex(klibcluu::LogLevel::Info, "  Reserved low memory: 0x0-0x", LOW_MEMORY_RESERVE);

    // 2. Reserve kernel physical memory
    let kernel_start_frame = (kernel_phys_start / 4096) as usize;
    let kernel_end_frame = ((kernel_phys_end + 4095) / 4096) as usize;
    for frame in kernel_start_frame..kernel_end_frame {
        mark_used(frame);
    }
    klibcluu::log_hex(klibcluu::LogLevel::Info, "  Reserved kernel: 0x", kernel_phys_start);
    klibcluu::log_hex(klibcluu::LogLevel::Info, " to 0x", kernel_phys_end);

    // 3. Reserve initrd (initial ramdisk)
    if let Some((initrd_ptr, initrd_size)) = boot_info.initrd_location() {
        let initrd_start_frame = (initrd_ptr / 4096) as usize;
        let initrd_end_frame = ((initrd_ptr + initrd_size + 4095) / 4096) as usize;
        for frame in initrd_start_frame..initrd_end_frame {
            mark_used(frame);
        }
        klibcluu::log_hex(klibcluu::LogLevel::Info, "  Reserved initrd: 0x", initrd_ptr);
        klibcluu::log_hex(klibcluu::LogLevel::Info, " to 0x", initrd_ptr + initrd_size);
    }

    // 4. Reserve BOOTBOOT structure (includes memory map)
    let bootboot_phys = boot_info.info_structure_location();
    let bootboot_size = bootboot.size as u64;
    if bootboot_size > 0 {
        let bootboot_start_frame = (bootboot_phys / 4096) as usize;
        let bootboot_end_frame = ((bootboot_phys + bootboot_size + 4095) / 4096) as usize;
        for frame in bootboot_start_frame..bootboot_end_frame {
            mark_used(frame);
        }
        klibcluu::log_hex(klibcluu::LogLevel::Info, "  Reserved BOOTBOOT: 0x", bootboot_phys);
        klibcluu::log_hex(klibcluu::LogLevel::Info, " to 0x", bootboot_phys + bootboot_size);
    }

    // 5. Reserve framebuffer
    if let Some((fb_phys, fb_size)) = boot_info.framebuffer_location() {
        let fb_start_frame = (fb_phys / 4096) as usize;
        let fb_end_frame = ((fb_phys + fb_size + 4095) / 4096) as usize;
        for frame in fb_start_frame..fb_end_frame {
            mark_used(frame);
        }
        klibcluu::log_hex(klibcluu::LogLevel::Info, "  Reserved framebuffer: 0x", fb_phys);
        klibcluu::log_hex(klibcluu::LogLevel::Info, " to 0x", fb_phys + fb_size);
    }

    let used = USED_FRAMES.load(Ordering::SeqCst);
    klibcluu::log_dec(klibcluu::LogLevel::Info, "  PMM initialized: ", used as u64);
    klibcluu::log_dec(klibcluu::LogLevel::Info, " / ", total_free_frames as u64);
    klibcluu::info(" frames");
}

/// Allocate a physical frame
///
/// Returns the physical address of a free 4KB frame, or None if out of memory
pub fn alloc_frame() -> Option<u64> {
    unsafe {
        for i in 0..FRAME_BITMAP.len() {
            if FRAME_BITMAP[i] != 0 {
                // Find first set bit
                let bit = FRAME_BITMAP[i].trailing_zeros() as usize;
                if bit < 64 {
                    let frame = i * 64 + bit;
                    // Safety check: never allocate frame 0 (NULL protection)
                    if frame == 0 {
                        klibcluu::error("  WARNING: Attempted to allocate frame 0, skipping!");
                        continue;
                    }
                    mark_used(frame);
                    let phys_addr = (frame * 4096) as u64;
                    // Verbose logging disabled for performance
                    // klibcluu::log_hex(klibcluu::LogLevel::Debug, "  PMM: Allocated frame ", frame as u64);
                    // klibcluu::log_hex(klibcluu::LogLevel::Debug, " @ phys 0x", phys_addr);
                    return Some(phys_addr);
                }
            }
        }
    }
    klibcluu::error("  PMM: Out of memory!");
    None
}

/// Free a physical frame
pub fn free_frame(phys_addr: u64) {
    let frame = (phys_addr / 4096) as usize;
    if frame < MAX_FRAMES {
        mark_free(frame);
    }
}

/// Mark a frame as used (allocated)
fn mark_used(frame: usize) {
    if frame >= MAX_FRAMES {
        return;
    }

    let index = frame / 64;
    let bit = frame % 64;
    let mask = 1u64 << bit;

    unsafe {
        if FRAME_BITMAP[index] & mask != 0 {
            // Was free, now marking as used
            FRAME_BITMAP[index] &= !mask;
            USED_FRAMES.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Mark a frame as free (available for allocation)
fn mark_free(frame: usize) {
    if frame >= MAX_FRAMES {
        return;
    }

    let index = frame / 64;
    let bit = frame % 64;
    let mask = 1u64 << bit;

    unsafe {
        if FRAME_BITMAP[index] & mask == 0 {
            // Was used, now marking as free
            FRAME_BITMAP[index] |= mask;

            // Don't decrement if we're initializing (USED_FRAMES starts at 0)
            if USED_FRAMES.load(Ordering::SeqCst) > 0 {
                USED_FRAMES.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

/// Get memory statistics
pub fn get_stats() -> (usize, usize) {
    let used = USED_FRAMES.load(Ordering::SeqCst);
    let total = TOTAL_FRAMES.load(Ordering::SeqCst);
    (used, total)
}
