//! Physical Memory Manager
//!
//! Manages physical frames using a bitmap allocator behind a spin lock.
//! Returns physical addresses that can be accessed via the physmap.

use crate::bootboot::{MMapEnt, BOOTBOOT};
use crate::mm::boot::info::BootInfoProvider;
use spin::Mutex;

/// Maximum number of physical frames we can track
/// For 4GB of RAM with 4KB pages = 1M frames = 128KB bitmap
const MAX_FRAMES: usize = 1024 * 1024; // 1M frames = 4GB

/// Number of u64 entries in the frame bitmap.
const FRAME_BITMAP_LEN: usize = MAX_FRAMES / 64;

/// Number of 4KB frames in a 2MB large page
const FRAMES_PER_LARGE_PAGE: usize = 512;

/// Bitmap allocator state.
///
/// Convention: bit SET = free, bit CLEAR = used.
struct FrameAllocator {
    bitmap: [u64; FRAME_BITMAP_LEN],
    total_frames: usize,
    used_frames: usize,
}

impl FrameAllocator {
    const fn new() -> Self {
        Self {
            bitmap: [0; FRAME_BITMAP_LEN],
            total_frames: 0,
            used_frames: 0,
        }
    }

    fn mark_used(&mut self, frame: usize) {
        if frame >= MAX_FRAMES {
            return;
        }
        let index = frame / 64;
        let mask = 1u64 << (frame % 64);
        if self.bitmap[index] & mask != 0 {
            self.bitmap[index] &= !mask;
            self.used_frames += 1;
        }
    }

    fn mark_free(&mut self, frame: usize) {
        if frame >= MAX_FRAMES {
            return;
        }
        let index = frame / 64;
        let mask = 1u64 << (frame % 64);
        if self.bitmap[index] & mask == 0 {
            self.bitmap[index] |= mask;
            if self.used_frames > 0 {
                self.used_frames -= 1;
            }
        }
    }

    fn alloc_frame(&mut self) -> Option<u64> {
        for i in 0..FRAME_BITMAP_LEN {
            let entry = self.bitmap[i];
            if entry != 0 {
                let bit = entry.trailing_zeros() as usize;
                let frame = i * 64 + bit;
                if frame == 0 {
                    continue;
                }
                self.mark_used(frame);
                return Some((frame * 4096) as u64);
            }
        }
        klibcluu::error("  PMM: Out of memory!");
        None
    }

    fn free_frame(&mut self, phys_addr: u64) {
        let frame = (phys_addr / 4096) as usize;
        if frame < MAX_FRAMES {
            self.mark_free(frame);
        }
    }

    fn alloc_large_frame(&mut self) -> Option<u64> {
        let mut start_frame = FRAMES_PER_LARGE_PAGE;

        while start_frame + FRAMES_PER_LARGE_PAGE <= MAX_FRAMES {
            let mut all_free = true;
            for i in 0..FRAMES_PER_LARGE_PAGE {
                let frame = start_frame + i;
                let index = frame / 64;
                let bit = frame % 64;
                if self.bitmap[index] & (1u64 << bit) == 0 {
                    all_free = false;
                    break;
                }
            }

            if all_free {
                for i in 0..FRAMES_PER_LARGE_PAGE {
                    self.mark_used(start_frame + i);
                }
                let phys_addr = (start_frame * 4096) as u64;
                return Some(phys_addr);
            }

            start_frame += FRAMES_PER_LARGE_PAGE;
        }

        klibcluu::warn("PMM: No contiguous 2MB region available");
        None
    }

    fn free_large_frame(&mut self, phys_addr: u64) {
        if phys_addr & 0x1FFFFF != 0 {
            klibcluu::warn("PMM: Attempted to free unaligned large frame");
            return;
        }
        let start_frame = (phys_addr / 4096) as usize;
        if start_frame + FRAMES_PER_LARGE_PAGE > MAX_FRAMES {
            return;
        }
        for i in 0..FRAMES_PER_LARGE_PAGE {
            self.mark_free(start_frame + i);
        }
    }
}

static PMM: Mutex<FrameAllocator> = Mutex::new(FrameAllocator::new());

/// Initialize the physical memory manager
///
/// Parses BOOTBOOT memory map and marks free regions in the bitmap.
/// Reserves all system regions: low memory, kernel, initrd, BOOTBOOT, framebuffer.
///
/// # Safety
///
/// Must run exactly once during boot before any allocations occur.
pub unsafe fn init(bootboot: &BOOTBOOT, boot_info: &dyn BootInfoProvider) {
    klibcluu::info("Initializing Physical Memory Manager...");

    let mut pmm = PMM.lock();

    let (kernel_phys_start, kernel_phys_end) = boot_info.kernel_physical_range();

    // Parse memory map and mark free regions
    let mmap_entries = bootboot.size as usize / 16;
    let mmap_ptr = &bootboot.mmap as *const MMapEnt;

    let mut total_free_frames = 0;

    for i in 0..mmap_entries {
        let entry = unsafe { &*mmap_ptr.add(i) };
        let entry_type = entry.size & 0xF;
        let size = entry.size & !0xF;

        // Only mark usable memory as free (type 1 = MMAP_FREE)
        if entry_type == 1 {
            let start_addr = entry.ptr;
            let end_addr = start_addr + size;
            let start_frame = ((start_addr / 4096) as usize).max(1);
            let end_frame = end_addr.div_ceil(4096) as usize;

            for frame in start_frame..end_frame.min(MAX_FRAMES) {
                pmm.mark_free(frame);
                total_free_frames += 1;
            }
        }
    }

    pmm.total_frames = total_free_frames;

    // ===== RESERVE SYSTEM REGIONS =====

    // 1. Reserve first 1 MB (NULL page, IVT, BIOS data area)
    const LOW_MEMORY_RESERVE: u64 = 0x100000;
    for frame in 0..(LOW_MEMORY_RESERVE / 4096) as usize {
        pmm.mark_used(frame);
    }

    // 2. Reserve kernel physical memory
    let kernel_start_frame = (kernel_phys_start / 4096) as usize;
    let kernel_end_frame = kernel_phys_end.div_ceil(4096) as usize;
    for frame in kernel_start_frame..kernel_end_frame {
        pmm.mark_used(frame);
    }

    // 3. Reserve initrd
    if let Some((initrd_ptr, initrd_size)) = boot_info.initrd_location() {
        let initrd_start_frame = (initrd_ptr / 4096) as usize;
        let initrd_end_frame = (initrd_ptr + initrd_size).div_ceil(4096) as usize;
        for frame in initrd_start_frame..initrd_end_frame {
            pmm.mark_used(frame);
        }
    }

    // 4. Reserve BOOTBOOT structure
    let bootboot_phys = boot_info.info_structure_location();
    let bootboot_size = bootboot.size as u64;
    if bootboot_size > 0 {
        let bootboot_start_frame = (bootboot_phys / 4096) as usize;
        let bootboot_end_frame = (bootboot_phys + bootboot_size).div_ceil(4096) as usize;
        for frame in bootboot_start_frame..bootboot_end_frame {
            pmm.mark_used(frame);
        }
    }

    // 5. Reserve framebuffer
    if let Some((fb_phys, fb_size)) = boot_info.framebuffer_location() {
        let fb_start_frame = (fb_phys / 4096) as usize;
        let fb_end_frame = (fb_phys + fb_size).div_ceil(4096) as usize;
        for frame in fb_start_frame..fb_end_frame {
            pmm.mark_used(frame);
        }
    }

    klibcluu::log_dec(
        klibcluu::LogLevel::Trace,
        "  PMM initialized: ",
        pmm.used_frames as u64,
    );
    klibcluu::log_dec(
        klibcluu::LogLevel::Trace,
        " / ",
        total_free_frames as u64,
    );
    klibcluu::trace(" frames");
}

/// Allocate a physical frame
///
/// Returns the physical address of a free 4KB frame, or None if out of memory.
pub fn alloc_frame() -> Option<u64> {
    PMM.lock().alloc_frame()
}

/// Free a physical frame
pub fn free_frame(phys_addr: u64) {
    PMM.lock().free_frame(phys_addr);
}

/// Allocate a 2MB-aligned contiguous block of physical memory (large page)
///
/// Returns the physical address of the start of the 2MB block, or None if
/// no suitable contiguous region is available.
pub fn alloc_large_frame() -> Option<u64> {
    PMM.lock().alloc_large_frame()
}

/// Free a 2MB large frame
pub fn free_large_frame(phys_addr: u64) {
    PMM.lock().free_large_frame(phys_addr);
}

/// Get memory statistics (used, total)
pub fn get_stats() -> (usize, usize) {
    let pmm = PMM.lock();
    (pmm.used_frames, pmm.total_frames)
}
