/*
 * Physical Memory Manager (PMM) - Dynamic Bitmap Allocator
 *
 * This module implements a bitmap-based physical frame allocator that scales
 * dynamically based on available RAM, with no hardcoded limits.
 *
 * KEY IMPROVEMENTS over old PMM:
 * - No MAX_FRAMES limit: scales to arbitrary RAM sizes
 * - Driven by BOOTBOOT memory map exclusively
 * - Proper reservation of all system regions
 * - Bootstrap-safe initialization
 *
 * DESIGN:
 * - Bitmap stored in dynamically allocated frames
 * - Each bit represents one 4 KiB frame (0=free, 1=used)
 * - Bootstrap uses small static bitmap, then migrates to dynamic
 * - Thread-safe via spin mutex
 */

use crate::bootboot::{BOOTBOOT, MMAP_FREE, MMapEnt};
use crate::memory::types::PhysFrame;
use spin::Mutex;

/// Bootstrap bitmap for early allocation (covers first 128 MB)
/// This allows us to allocate frames for the real bitmap and page tables
const BOOTSTRAP_FRAMES: usize = 32768; // 128 MB / 4 KiB
const BOOTSTRAP_WORDS: usize = BOOTSTRAP_FRAMES / 64;
static mut BOOTSTRAP_BITMAP: [u64; BOOTSTRAP_WORDS] = [!0; BOOTSTRAP_WORDS];

/// Dynamic bitmap pointer (set after bootstrap)
static mut DYNAMIC_BITMAP: Option<*mut u64> = None;
static mut DYNAMIC_BITMAP_WORDS: usize = 0;

/// Total number of frames in the system
static mut TOTAL_FRAMES: usize = 0;

/// Lock for thread-safe access
static ALLOCATOR_LOCK: Mutex<()> = Mutex::new(());

/// Bootstrap mode flag
static mut BOOTSTRAP_MODE: bool = true;

unsafe extern "C" {
    static __text_start: u8;
    static __bss_end: u8;
}

/// Initialize PMM from BOOTBOOT memory map
///
/// This performs a multi-stage initialization:
/// 1. Parse memory map to find max physical address
/// 2. Mark all frames as used initially (conservative)
/// 3. Mark free regions from memory map
/// 4. Reserve system regions (kernel, initrd, BOOTBOOT structures)
/// 5. Allocate and set up dynamic bitmap if needed
///
/// # Safety
/// Must be called exactly once during boot with valid BOOTBOOT pointer
pub unsafe fn init(bootboot_ptr: *const BOOTBOOT, kernel_phys_base: u64, bootboot_phys: u64) {
    unsafe {
        let _lock = ALLOCATOR_LOCK.lock();

        log::info!("Initializing physical memory manager...");
        log::info!("Kernel physical base: {:#x}", kernel_phys_base);
        log::info!("BOOTBOOT physical base: {:#x}", bootboot_phys);

        let bootboot = &*bootboot_ptr;

        // Step 1: Parse memory map to find total RAM
        let max_phys = parse_memory_map_max(bootboot);
        let total_frames = (max_phys / PhysFrame::SIZE) as usize;
        TOTAL_FRAMES = total_frames;

        log::info!(
            "Detected {} frames ({} MB) of physical memory",
            total_frames,
            (total_frames * 4096) / (1024 * 1024)
        );

        // Step 2: Calculate bitmap size needed
        let bitmap_words = (total_frames + 63) / 64;
        let bitmap_bytes = bitmap_words * 8;
        let bitmap_frames = (bitmap_bytes + 4095) / 4096;

        log::info!(
            "Bitmap requires {} frames ({} KB)",
            bitmap_frames,
            (bitmap_frames * 4096) / 1024
        );

        // Step 3: Initialize bootstrap bitmap (all used initially)
        for i in 0..BOOTSTRAP_WORDS {
            *core::ptr::addr_of_mut!(BOOTSTRAP_BITMAP)
                .cast::<u64>()
                .add(i) = u64::MAX;
        }

        // Step 4: Mark free regions in bootstrap bitmap from memory map
        mark_free_regions_bootstrap(bootboot);

        // Step 5: Reserve system regions in bootstrap bitmap
        reserve_system_regions_bootstrap(bootboot, kernel_phys_base, bootboot_phys);

        // Step 6: If we need more than bootstrap covers, allocate dynamic bitmap
        if total_frames > BOOTSTRAP_FRAMES {
            log::info!(
                "Allocating dynamic bitmap ({} frames needed)...",
                bitmap_frames
            );

            // Allocate contiguous frames for dynamic bitmap
            // This is critical - the bitmap MUST be contiguous to avoid holes
            let bitmap_phys_start = alloc_contiguous_bootstrap(bitmap_frames)
                .expect("Failed to allocate contiguous frames for bitmap");

            let bitmap_ptr = bitmap_phys_start as *mut u64;

            log::info!(
                "Allocated contiguous bitmap at phys 0x{:x}-0x{:x}",
                bitmap_phys_start,
                bitmap_phys_start + (bitmap_frames as u64 * PhysFrame::SIZE)
            );

            // Initialize dynamic bitmap (all used initially)
            // Access via identity mapping - BOOTBOOT maps all physical RAM
            for i in 0..bitmap_words {
                *bitmap_ptr.add(i) = u64::MAX;
            }

            // Mark free regions in dynamic bitmap from memory map
            mark_free_regions_dynamic(bootboot, bitmap_ptr, bitmap_words);

            // Reserve system regions in dynamic bitmap
            reserve_system_regions_dynamic(
                bootboot,
                bitmap_ptr,
                bitmap_words,
                kernel_phys_base,
                bootboot_phys,
            );

            // Mark bitmap frames themselves as used
            for i in 0..bitmap_frames {
                let frame_addr = bitmap_phys_start + (i as u64 * PhysFrame::SIZE);
                let frame_num = (frame_addr / PhysFrame::SIZE) as usize;
                mark_frame_used_in_bitmap(bitmap_ptr, bitmap_words, frame_num);
            }

            // Switch to dynamic mode
            DYNAMIC_BITMAP = Some(bitmap_ptr);
            DYNAMIC_BITMAP_WORDS = bitmap_words;
            BOOTSTRAP_MODE = false;

            log::info!(
                "Switched to dynamic bitmap ({} words for {} frames)",
                bitmap_words,
                total_frames
            );
        } else {
            log::info!("Using bootstrap bitmap (system has <= 128 MB RAM)");
            // Stay in bootstrap mode
        }

        let (used, total) = get_stats_internal();
        log::info!(
            "PMM initialized: {} / {} frames used ({} MB / {} MB)",
            used,
            total,
            (used * 4096) / (1024 * 1024),
            (total * 4096) / (1024 * 1024)
        );
    }
}

/// Update bitmap pointer to use physmap after CR3 switch
///
/// CRITICAL: This must be called after switching to our own page tables!
/// The bitmap was initially accessed via BOOTBOOT's identity mapping.
/// After CR3 switch, we need to access it via the physmap instead.
pub unsafe fn update_bitmap_for_new_pagetables() {
    use crate::memory::types::PhysAddr;

    unsafe {
        if let Some(old_ptr) = DYNAMIC_BITMAP {
            // Convert physical address (old pointer) to physmap virtual address
            let phys_addr_u64 = old_ptr as u64;
            let phys_addr = PhysAddr::new(phys_addr_u64);
            let physmap_virt = crate::memory::physmap::phys_to_virt(phys_addr);
            let new_ptr = physmap_virt.as_u64() as *mut u64;

            DYNAMIC_BITMAP = Some(new_ptr);

            log::debug!(
                "Updated bitmap pointer: phys 0x{:x} -> virt 0x{:x}",
                phys_addr_u64,
                physmap_virt.as_u64()
            );
        }
    }
}

/// Parse BOOTBOOT memory map to find maximum physical address
unsafe fn parse_memory_map_max(bootboot: &BOOTBOOT) -> u64 {
    unsafe {
        let mmap_entries = get_mmap_entries(bootboot);
        let mmap_base: *const MMapEnt = core::ptr::addr_of!(bootboot.mmap);

        let mut max_addr = 0u64;

        for i in 0..mmap_entries {
            let entry = &*mmap_base.add(i);
            let region_ptr = entry.ptr;
            let raw_size = entry.size;
            let region_size = raw_size & !0xF;

            if region_size > 0 {
                let end_addr = region_ptr + region_size;
                if end_addr > max_addr {
                    max_addr = end_addr;
                }
            }
        }

        // Round up to frame boundary
        (max_addr + PhysFrame::SIZE - 1) & !(PhysFrame::SIZE - 1)
    }
}

/// Get number of memory map entries
fn get_mmap_entries(bootboot: &BOOTBOOT) -> usize {
    let bb_size = bootboot.size as usize;
    let total_bytes = bb_size.saturating_sub(128);
    total_bytes / core::mem::size_of::<MMapEnt>()
}

/// Mark free regions from memory map in bootstrap bitmap
unsafe fn mark_free_regions_bootstrap(bootboot: &BOOTBOOT) {
    unsafe {
        let mmap_entries = get_mmap_entries(bootboot);
        let mmap_base: *const MMapEnt = core::ptr::addr_of!(bootboot.mmap);

        for i in 0..mmap_entries {
            let entry = &*mmap_base.add(i);
            let region_ptr = entry.ptr;
            let raw_size = entry.size;
            let entry_type = (raw_size & 0xF) as u32;
            let region_size = raw_size & !0xF;

            if region_size == 0 {
                continue;
            }

            // Only mark FREE regions
            if entry_type == MMAP_FREE {
                let start_frame = (region_ptr / PhysFrame::SIZE) as usize;
                let end_frame = ((region_ptr + region_size - 1) / PhysFrame::SIZE) as usize;

                for frame_num in start_frame..=end_frame {
                    if frame_num < BOOTSTRAP_FRAMES {
                        mark_frame_free_in_bitmap(
                            core::ptr::addr_of_mut!(BOOTSTRAP_BITMAP).cast::<u64>(),
                            BOOTSTRAP_WORDS,
                            frame_num,
                        );
                    }
                }
            }
        }
    }
}

/// Mark free regions from memory map in dynamic bitmap
unsafe fn mark_free_regions_dynamic(bootboot: &BOOTBOOT, bitmap: *mut u64, bitmap_words: usize) {
    unsafe {
        let mmap_entries = get_mmap_entries(bootboot);
        let mmap_base: *const MMapEnt = core::ptr::addr_of!(bootboot.mmap);

        for i in 0..mmap_entries {
            let entry = &*mmap_base.add(i);
            let region_ptr = entry.ptr;
            let raw_size = entry.size;
            let entry_type = (raw_size & 0xF) as u32;
            let region_size = raw_size & !0xF;

            if region_size == 0 {
                continue;
            }

            // Only mark FREE regions
            if entry_type == MMAP_FREE {
                let start_frame = (region_ptr / PhysFrame::SIZE) as usize;
                let end_frame = ((region_ptr + region_size - 1) / PhysFrame::SIZE) as usize;

                for frame_num in start_frame..=end_frame {
                    mark_frame_free_in_bitmap(bitmap, bitmap_words, frame_num);
                }
            }
        }
    }
}

/// Reserve system regions (kernel, initrd, BOOTBOOT structures) in bootstrap bitmap
unsafe fn reserve_system_regions_bootstrap(
    bootboot: &BOOTBOOT,
    kernel_phys_base: u64,
    bootboot_phys: u64,
) {
    unsafe {
        // Reserve first 1 MB (NULL page, real mode IVT, BIOS data area)
        // This is critical for NULL pointer safety and legacy compatibility
        const LOW_MEMORY_RESERVE: u64 = 0x100000; // 1 MB
        reserve_range_bootstrap(0, LOW_MEMORY_RESERVE);
        log::info!(
            "Reserved low memory: phys 0x0-0x{:x} (NULL page, IVT, BIOS)",
            LOW_MEMORY_RESERVE
        );

        // Reserve kernel physical range
        let kernel_virt_start = core::ptr::addr_of!(__text_start) as u64;
        let kernel_virt_end = core::ptr::addr_of!(__bss_end) as u64;
        let kernel_phys_start = kernel_virt_start - 0xffffffffffe02000 + kernel_phys_base;
        let kernel_phys_end = kernel_virt_end - 0xffffffffffe02000 + kernel_phys_base;

        reserve_range_bootstrap(kernel_phys_start, kernel_phys_end);
        log::info!(
            "Reserved kernel: phys 0x{:x}-0x{:x}",
            kernel_phys_start,
            kernel_phys_end
        );

        // Reserve initrd
        let initrd_ptr = bootboot.initrd_ptr;
        let initrd_size = bootboot.initrd_size as u64;
        if initrd_size > 0 {
            reserve_range_bootstrap(initrd_ptr, initrd_ptr + initrd_size);
            log::info!(
                "Reserved initrd: phys 0x{:x}-0x{:x} ({} KB)",
                initrd_ptr,
                initrd_ptr + initrd_size,
                initrd_size / 1024
            );
        }

        // Reserve BOOTBOOT info structure (includes mmap)
        let _bootboot_ptr_addr = bootboot as *const BOOTBOOT as u64;
        let bootboot_size = bootboot.size as u64;
        reserve_range_bootstrap(bootboot_phys, bootboot_phys + bootboot_size);
        log::info!(
            "Reserved BOOTBOOT struct: phys 0x{:x}-0x{:x}",
            bootboot_phys,
            bootboot_phys + bootboot_size
        );

        // Reserve framebuffer (if present)
        let fb_ptr = bootboot.fb_ptr as u64;
        let fb_size = bootboot.fb_size as u64;
        if fb_size > 0 && fb_ptr != 0 {
            // fb_ptr is virtual, need to find physical address
            // For now, assume it's identity-mapped by BOOTBOOT or in high memory
            // We'll handle this more carefully later
            log::info!(
                "Framebuffer at virt 0x{:x}, size {} KB",
                fb_ptr,
                fb_size / 1024
            );
        }
    }
}

/// Reserve system regions in dynamic bitmap
unsafe fn reserve_system_regions_dynamic(
    bootboot: &BOOTBOOT,
    bitmap: *mut u64,
    bitmap_words: usize,
    kernel_phys_base: u64,
    bootboot_phys: u64,
) {
    unsafe {
        // Reserve first 1 MB (NULL page, real mode IVT, BIOS data area)
        const LOW_MEMORY_RESERVE: u64 = 0x100000; // 1 MB
        reserve_range_dynamic(0, LOW_MEMORY_RESERVE, bitmap, bitmap_words);

        // Reserve kernel
        let kernel_virt_start = core::ptr::addr_of!(__text_start) as u64;
        let kernel_virt_end = core::ptr::addr_of!(__bss_end) as u64;
        let kernel_phys_start = kernel_virt_start - 0xffffffffffe02000 + kernel_phys_base;
        let kernel_phys_end = kernel_virt_end - 0xffffffffffe02000 + kernel_phys_base;

        reserve_range_dynamic(kernel_phys_start, kernel_phys_end, bitmap, bitmap_words);

        // Reserve initrd
        let initrd_ptr = bootboot.initrd_ptr;
        let initrd_size = bootboot.initrd_size as u64;
        if initrd_size > 0 {
            reserve_range_dynamic(initrd_ptr, initrd_ptr + initrd_size, bitmap, bitmap_words);
        }

        // Reserve BOOTBOOT struct
        let bootboot_size = bootboot.size as u64;
        reserve_range_dynamic(
            bootboot_phys,
            bootboot_phys + bootboot_size,
            bitmap,
            bitmap_words,
        );
    }
}

/// Reserve a physical address range in bootstrap bitmap [start, end)
unsafe fn reserve_range_bootstrap(start: u64, end: u64) {
    unsafe {
        let start_frame = (start / PhysFrame::SIZE) as usize;
        let end_frame = ((end + PhysFrame::SIZE - 1) / PhysFrame::SIZE) as usize;

        for frame_num in start_frame..end_frame {
            if frame_num < BOOTSTRAP_FRAMES {
                mark_frame_used_in_bitmap(
                    core::ptr::addr_of_mut!(BOOTSTRAP_BITMAP).cast::<u64>(),
                    BOOTSTRAP_WORDS,
                    frame_num,
                );
            }
        }
    }
}

/// Reserve a physical address range in dynamic bitmap [start, end)
unsafe fn reserve_range_dynamic(start: u64, end: u64, bitmap: *mut u64, bitmap_words: usize) {
    unsafe {
        let start_frame = (start / PhysFrame::SIZE) as usize;
        let end_frame = ((end + PhysFrame::SIZE - 1) / PhysFrame::SIZE) as usize;

        for frame_num in start_frame..end_frame {
            mark_frame_used_in_bitmap(bitmap, bitmap_words, frame_num);
        }
    }
}

/// Allocate a frame using bootstrap allocator
unsafe fn alloc_frame_bootstrap() -> Option<PhysFrame> {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(BOOTSTRAP_BITMAP).cast::<u64>();

        for word_idx in 0..BOOTSTRAP_WORDS {
            let word_val = *ptr.add(word_idx);

            if word_val != u64::MAX {
                for bit_idx in 0..64 {
                    let mask = 1u64 << bit_idx;
                    if (word_val & mask) == 0 {
                        *ptr.add(word_idx) = word_val | mask;
                        let frame_num = word_idx * 64 + bit_idx;
                        let frame_addr = (frame_num as u64) * PhysFrame::SIZE;
                        return Some(PhysFrame::containing_address(frame_addr));
                    }
                }
            }
        }

        None
    }
}

/// Free a frame using bootstrap allocator
unsafe fn free_frame_bootstrap(frame: PhysFrame) {
    unsafe {
        let frame_num = (frame.start_address() / PhysFrame::SIZE) as usize;
        if frame_num < BOOTSTRAP_FRAMES {
            mark_frame_free_in_bitmap(
                core::ptr::addr_of_mut!(BOOTSTRAP_BITMAP).cast::<u64>(),
                BOOTSTRAP_WORDS,
                frame_num,
            );
        }
    }
}

/// Allocate N contiguous frames using bootstrap allocator
///
/// Scans the bootstrap bitmap for a contiguous run of N free frames.
/// This is critical for structures like the dynamic bitmap that must be
/// contiguous to avoid reading/writing into reserved regions (ACPI, MMIO).
///
/// Returns the physical address of the first frame, or None if no contiguous
/// block of the requested size is available.
unsafe fn alloc_contiguous_bootstrap(count: usize) -> Option<u64> {
    unsafe {
        if count == 0 || count > BOOTSTRAP_FRAMES {
            return None;
        }

        let ptr = core::ptr::addr_of_mut!(BOOTSTRAP_BITMAP).cast::<u64>();

        // Search for 'count' consecutive free frames
        let mut consecutive_free = 0;
        let mut start_frame = 0;

        for frame_num in 0..BOOTSTRAP_FRAMES {
            let word_idx = frame_num / 64;
            let bit_idx = frame_num % 64;
            let word = *ptr.add(word_idx);
            let mask = 1u64 << bit_idx;

            if (word & mask) == 0 {
                // Frame is free
                if consecutive_free == 0 {
                    start_frame = frame_num;
                }
                consecutive_free += 1;

                if consecutive_free == count {
                    // Found enough consecutive frames!
                    // Mark them all as used
                    for i in 0..count {
                        let f = start_frame + i;
                        mark_frame_used_in_bitmap(ptr, BOOTSTRAP_WORDS, f);
                    }

                    let phys_addr = (start_frame as u64) * PhysFrame::SIZE;
                    return Some(phys_addr);
                }
            } else {
                // Frame is used, reset counter
                consecutive_free = 0;
            }
        }

        None // Couldn't find contiguous block
    }
}

/// Mark a frame as free in bitmap
unsafe fn mark_frame_free_in_bitmap(bitmap: *mut u64, bitmap_words: usize, frame_num: usize) {
    unsafe {
        let word_idx = frame_num / 64;
        let bit_idx = frame_num % 64;

        if word_idx < bitmap_words {
            let mask = 1u64 << bit_idx;
            let ptr = bitmap.add(word_idx);
            let val = *ptr;
            *ptr = val & !mask;
        }
    }
}

/// Mark a frame as used in bitmap
unsafe fn mark_frame_used_in_bitmap(bitmap: *mut u64, bitmap_words: usize, frame_num: usize) {
    unsafe {
        let word_idx = frame_num / 64;
        let bit_idx = frame_num % 64;

        if word_idx < bitmap_words {
            let mask = 1u64 << bit_idx;
            let ptr = bitmap.add(word_idx);
            let val = *ptr;
            *ptr = val | mask;
        }
    }
}

/// Public API: Allocate a physical frame
pub fn alloc_frame() -> Option<PhysFrame> {
    let _lock = ALLOCATOR_LOCK.lock();

    unsafe {
        if BOOTSTRAP_MODE {
            alloc_frame_bootstrap()
        } else {
            alloc_frame_dynamic()
        }
    }
}

/// Allocate a frame using dynamic bitmap
unsafe fn alloc_frame_dynamic() -> Option<PhysFrame> {
    unsafe {
        let bitmap = DYNAMIC_BITMAP?;
        let bitmap_words = DYNAMIC_BITMAP_WORDS;

        for word_idx in 0..bitmap_words {
            let word_val = *bitmap.add(word_idx);

            if word_val != u64::MAX {
                for bit_idx in 0..64 {
                    let mask = 1u64 << bit_idx;
                    if (word_val & mask) == 0 {
                        *bitmap.add(word_idx) = word_val | mask;
                        let frame_num = word_idx * 64 + bit_idx;
                        if frame_num < TOTAL_FRAMES {
                            let frame_addr = (frame_num as u64) * PhysFrame::SIZE;
                            return Some(PhysFrame::containing_address(frame_addr));
                        }
                    }
                }
            }
        }

        None
    }
}

/// Public API: Free a physical frame
pub fn free_frame(frame: PhysFrame) {
    let _lock = ALLOCATOR_LOCK.lock();

    let frame_num = (frame.start_address() / PhysFrame::SIZE) as usize;

    unsafe {
        if BOOTSTRAP_MODE {
            free_frame_bootstrap(frame);
        } else {
            if let Some(bitmap) = DYNAMIC_BITMAP {
                mark_frame_free_in_bitmap(bitmap, DYNAMIC_BITMAP_WORDS, frame_num);
            }
        }
    }
}

/// Public API: Get memory statistics
pub fn get_stats() -> (usize, usize) {
    let _lock = ALLOCATOR_LOCK.lock();
    unsafe { get_stats_internal() }
}

/// Internal: Get memory statistics (must hold lock)
unsafe fn get_stats_internal() -> (usize, usize) {
    unsafe {
        let total = TOTAL_FRAMES;
        let mut used = 0;

        if BOOTSTRAP_MODE {
            let words_to_scan = (total + 63) / 64;
            let words_to_scan = words_to_scan.min(BOOTSTRAP_WORDS);

            for i in 0..words_to_scan {
                let word = *core::ptr::addr_of!(BOOTSTRAP_BITMAP).cast::<u64>().add(i);
                if i == words_to_scan - 1 {
                    let bits_in_last = total % 64;
                    if bits_in_last > 0 {
                        let mask = (1u64 << bits_in_last) - 1;
                        used += (word & mask).count_ones() as usize;
                    } else {
                        used += word.count_ones() as usize;
                    }
                } else {
                    used += word.count_ones() as usize;
                }
            }
        } else {
            if let Some(bitmap) = DYNAMIC_BITMAP {
                let words_to_scan = (total + 63) / 64;

                for i in 0..words_to_scan {
                    let word = *bitmap.add(i);
                    if i == words_to_scan - 1 {
                        let bits_in_last = total % 64;
                        if bits_in_last > 0 {
                            let mask = (1u64 << bits_in_last) - 1;
                            used += (word & mask).count_ones() as usize;
                        } else {
                            used += word.count_ones() as usize;
                        }
                    } else {
                        used += word.count_ones() as usize;
                    }
                }
            }
        }

        (used, total)
    }
}

/// Public API: Reserve a physical address range [start, end)
///
/// This is used to mark regions as used that weren't in the BOOTBOOT mmap
/// or need additional reservation.
pub fn reserve_range(start_addr: u64, end_addr: u64) {
    let _lock = ALLOCATOR_LOCK.lock();

    unsafe {
        if BOOTSTRAP_MODE {
            reserve_range_bootstrap(start_addr, end_addr);
        } else {
            if let Some(bitmap) = DYNAMIC_BITMAP {
                reserve_range_dynamic(start_addr, end_addr, bitmap, DYNAMIC_BITMAP_WORDS);
            }
        }
    }

    log::debug!(
        "Reserved physical range 0x{:x}-0x{:x}",
        start_addr,
        end_addr
    );
}
