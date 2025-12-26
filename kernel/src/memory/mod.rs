/*
 * Memory Management (New Implementation)
 *
 * This is the top-level memory management module that coordinates all aspects
 * of kernel memory management using the new physmap-based architecture.
 *
 * ARCHITECTURE OVERVIEW:
 *
 * 1. Physical Memory Management (pmm_new module):
 *    - Manages 4 KiB physical memory frames with dynamic bitmap
 *    - No hardcoded MAX_FRAMES limit - scales to arbitrary RAM
 *    - Bootstrap mode for initial allocation, then migrates to dynamic bitmap
 *    - Reserves kernel, initrd, BOOTBOOT structures automatically
 *
 * 2. Physmap (physmap module):
 *    - Direct map of all physical memory at fixed high-half address
 *    - Base: 0xffff_8000_0000_0000
 *    - Allows access to any physical address without CR3 switching
 *    - Foundation for clean page table manipulation
 *
 * 3. Virtual Memory Management (paging_new module):
 *    - Manages page tables using physmap access
 *    - Eliminates CR3 switching hacks
 *    - Supports creating/manipulating page tables for any root PhysAddr
 *    - Provides map_4k, unmap_4k, translate APIs
 *
 * 4. Address Spaces (address_space_new module):
 *    - Per-process page tables with kernel half shared
 *    - build_kernel_space() creates fresh PML4 with kernel mappings
 *    - new_user() allocates PML4 and copies kernel half
 *    - User/kernel isolation with proper permissions
 *
 * 5. Kernel Heap (heap module):
 *    - Dynamic memory allocation (Box, Vec, etc.)
 *    - Built on top of new paging API
 *    - Mapped in all address spaces (kernel half)
 *
 * INITIALIZATION SEQUENCE:
 * 1. Detect kernel and BOOTBOOT physical addresses via page table walk
 * 2. Parse BOOTBOOT memory map → determine max_phys
 * 3. PMM init → dynamic bitmap, reserve system regions
 * 4. Physmap init → enable phys_to_virt conversions
 * 5. Build kernel address space → new PML4 with kernel mappings + physmap
 * 6. Switch CR3 → take ownership from BOOTBOOT
 * 7. Heap init → map kernel heap using new paging API
 *
 * MEMORY LAYOUT:
 * - Physmap: 0xffff_8000_0000_0000 (maps all physical memory)
 * - Kernel code: 0xffff_ffff_ffe02000 (from linker script)
 * - Kernel heap: 0xffff_ffff_c0000000 (8 MiB)
 * - BOOTBOOT structures: 0xffff_ffff_ffe00000
 * - User space: 0x00000000 - 0x80000000 (per-process)
 */

// New modules
pub mod address_space;
pub mod paging; // This is the renamed paging_new.rs (now public)
pub mod phys; // This is the renamed pmm_new.rs (now public)
pub mod physmap;
pub mod types; // This is the renamed address_space_new.rs

// Old modules (kept temporarily for compatibility)
pub mod heap;

// Re-export types
pub use address_space::AddressSpace;
use alloc::format;
pub use types::{PageTableFlags, PhysAddr, PhysFrame, VirtAddr};

use crate::bootboot::BOOTBOOT;

/// Physical memory manager API (re-export from phys module)
pub mod pmm {}

/// Paging API (re-export from paging module)
pub mod paging_api {}

/// Kernel address space singleton
///
/// This holds the kernel's page table root after we build fresh page tables.
/// Other modules may need this for copying kernel mappings to user spaces.
static mut KERNEL_ADDRESS_SPACE: Option<AddressSpace> = None;

/// Get the kernel address space
///
/// Returns a reference to the kernel's AddressSpace.
/// Panics if memory hasn't been initialized yet.
pub fn kernel_address_space() -> &'static AddressSpace {
    unsafe {
        (*core::ptr::addr_of!(KERNEL_ADDRESS_SPACE))
            .as_ref()
            .expect("Kernel address space not initialized")
    }
}

/// Top-level memory management initialization
///
/// Initializes all memory management subsystems in the correct order.
/// This replaces BOOTBOOT's page tables with our own fresh tables.
///
/// # Arguments
/// * `bootboot_ptr` - Pointer to BOOTBOOT structure containing memory map
///
/// # Initialization Steps
/// 1. Detect kernel and BOOTBOOT physical addresses by walking page tables
/// 2. Parse BOOTBOOT memory map to find max physical address
/// 3. Initialize PMM with dynamic bitmap
/// 4. Initialize physmap for phys↔virt conversions
/// 5. Build kernel address space (new PML4 with kernel mappings + physmap)
/// 6. Switch CR3 to new kernel page tables
/// 7. Initialize kernel heap
///
/// # Safety
/// Must be called exactly once during boot, after BOOTBOOT handoff.
/// The caller must ensure bootboot_ptr points to a valid BOOTBOOT structure.
pub unsafe fn init(bootboot_ptr: *const BOOTBOOT) {
    log::info!("=== Memory Management Initialization (New Architecture) ===");

    let max_phys = unsafe {
        let bootboot = &*bootboot_ptr;

        // Step 1: Detect kernel and BOOTBOOT physical addresses
        // This must be done while we're still using BOOTBOOT's page tables
        log::info!("Detecting physical addresses...");

        let kernel_phys_base = paging::detect_kernel_physical_base()
            .expect("Failed to detect kernel physical base address");

        // Detect BOOTBOOT physical address (it's at virtual 0xffffffffffe00000)
        let bootboot_virt = VirtAddr::new(bootboot_ptr as u64);
        let current_cr3 = paging::get_current_cr3();
        let bootboot_phys = paging::translate_via_identity(current_cr3, bootboot_virt)
            .expect("Failed to translate BOOTBOOT virtual address");

        log::info!("Kernel physical base: {:#x}", kernel_phys_base);
        log::info!("BOOTBOOT physical: {:#x}", bootboot_phys.as_u64());

        // Step 2: Calculate max physical address from BOOTBOOT memory map
        let max_phys = calculate_max_phys(bootboot);
        log::info!(
            "Max physical address: {:#x} ({} MB)",
            max_phys,
            max_phys / 1024 / 1024
        );

        // Step 3: Initialize physical memory manager
        // This parses the memory map, reserves system regions, and sets up the bitmap
        log::info!("Initializing physical memory manager...");
        phys::init(bootboot_ptr, kernel_phys_base, bootboot_phys.as_u64());
        let (used, total) = phys::get_stats();
        log::info!(
            "PMM initialized: {} / {} frames ({:.1}% used)",
            used,
            total,
            (used as f32 / total as f32) * 100.0
        );

        // Step 4: Initialize physmap (before building kernel space)
        // This allows phys_to_virt conversions during kernel space setup
        log::info!("Initializing physmap...");
        physmap::init(max_phys);

        // Step 5: Build kernel address space with fresh page tables
        // This creates a new PML4 and maps:
        // - Kernel code/data
        // - BOOTBOOT structures
        // - Framebuffer
        // - Physmap (all physical memory)
        log::info!("Building kernel address space...");
        let kernel_space = address_space::AddressSpace::build_kernel_space(
            bootboot_ptr,
            kernel_phys_base,
            bootboot_phys.as_u64(),
        )
        .expect("Failed to build kernel address space");

        log::info!(
            "Kernel PML4 at physical address: {:#x}",
            kernel_space.page_table_root.as_u64()
        );

        // Step 6: Switch to new kernel page tables
        // This takes ownership from BOOTBOOT's inherited page tables
        log::info!("Switching to new kernel page tables...");
        kernel_space.switch_to();
        // DO NOT log immediately after CR3 switch - might access unmapped memory!

        // Activate physmap - now it's safe to use physmap for physical memory access
        physmap::activate();

        // CRITICAL: Update PMM bitmap pointer to use physmap instead of identity mapping
        phys::update_bitmap_for_new_pagetables();

        // Now it's safe to log after physmap is active
        log::info!("CR3 switched successfully - now using our own page tables!");

        // Save kernel address space
        KERNEL_ADDRESS_SPACE = Some(kernel_space);

        max_phys
    };

    // Step 7: Initialize kernel heap
    // This must come last as it depends on frame allocation and paging
    log::info!("Initializing kernel heap...");
    heap::init().expect("Failed to initialize kernel heap");

    // Print final statistics
    let (used, total) = phys::get_stats();
    log::info!(
        "=== Memory initialization complete ===\n\
         Physical memory: {} / {} frames ({:.1}% used)\n\
         Kernel PML4: {:#x}\n\
         Physmap base: {:#x}\n\
         Max physical: {:#x} ({} MB)",
        used,
        total,
        (used as f32 / total as f32) * 100.0,
        kernel_address_space().page_table_root.as_u64(),
        physmap::PHYS_MAP_BASE,
        max_phys,
        max_phys / 1024 / 1024,
    );

    // Verification: test the new memory system
    verify_memory_system();

    // Display memory layout summary
    print_memory_layout();
}

/// Calculate maximum physical address from BOOTBOOT memory map
///
/// Scans all memory map entries and finds the highest physical address.
/// This is used to determine how much memory to map in the physmap.
fn calculate_max_phys(bootboot: &BOOTBOOT) -> u64 {
    use crate::bootboot::MMapEnt;

    let mmap_entries = bootboot.size as usize / 16; // Each entry is 16 bytes
    let mmap_ptr = &bootboot.mmap as *const MMapEnt;

    let mut max_addr = 0u64;

    for i in 0..mmap_entries {
        let entry = unsafe { &*mmap_ptr.add(i) };
        let start = entry.ptr;
        let size = entry.size & !0xf; // Clear type bits
        let end = start + size;

        if end > max_addr {
            max_addr = end;
        }
    }

    // Round up to next MB for safety
    (max_addr + 0xfffff) & !0xfffff
}

/// Verify memory system functionality
///
/// Performs basic sanity checks on the new memory system:
/// 1. Test frame allocation
/// 2. Test page mapping
/// 3. Test physical memory read/write via physmap
/// 4. Test virtual address translation
fn verify_memory_system() {
    log::info!("=== Memory System Verification ===");

    // Test 1: Allocate a frame
    let test_frame = phys::alloc_frame().expect("Failed to allocate test frame");
    log::info!(
        "✓ Frame allocation works: frame at {:#x}",
        test_frame.start_address()
    );

    // Test 2: Map a page
    let test_virt = VirtAddr::new(0xffff_ffff_a000_0000);
    let test_phys = PhysAddr::new(test_frame.start_address());

    let kernel_cr3 = kernel_address_space().page_table_root;

    paging::map_4k(
        kernel_cr3,
        test_virt,
        test_phys,
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
    )
    .expect("Failed to map test page");

    log::info!(
        "✓ Page mapping works: virt {:#x} → phys {:#x}",
        test_virt.as_u64(),
        test_phys.as_u64()
    );

    // Test 3: Write and read via mapped page
    unsafe {
        let ptr = test_virt.as_mut_ptr::<u64>();
        core::ptr::write_volatile(ptr, 0xdeadbeef_cafebabe);
        let value = core::ptr::read_volatile(ptr);
        assert_eq!(value, 0xdeadbeef_cafebabe, "Memory read/write failed");
    }
    log::info!("✓ Memory read/write works via mapped page");

    // Test 4: Translate address
    let translated =
        paging::translate(kernel_cr3, test_virt).expect("Failed to translate test address");
    assert_eq!(
        translated.0.as_u64(),
        test_phys.as_u64(),
        "Translation mismatch"
    );
    log::info!("✓ Address translation works");

    // Test 5: Access via physmap
    unsafe {
        let phys_value = physmap::read_phys::<u64>(test_phys);
        assert_eq!(phys_value, 0xdeadbeef_cafebabe, "Physmap read failed");
    }
    log::info!("✓ Physmap access works");

    // Cleanup
    paging::unmap_4k(kernel_cr3, test_virt).expect("Failed to unmap test page");
    phys::free_frame(test_frame);

    log::info!("=== All memory system tests passed! ===");
}

/// Print memory layout summary
fn print_memory_layout() {
    use crate::bootboot::bootboot;

    // Get linker symbols
    unsafe extern "C" {
        static __text_start: u8;
        static __bss_end: u8;
        static fb: u8;
    }

    let kernel_start = unsafe { &__text_start as *const _ as u64 };
    let kernel_end = unsafe { &__bss_end as *const _ as u64 };
    let fb_addr = unsafe { &fb as *const _ as u64 };

    let (used_frames, total_frames) = phys::get_stats();
    let used_mb = (used_frames * 4096) / (1024 * 1024);
    let total_mb = (total_frames * 4096) / (1024 * 1024);

    log::info!("");
    log::info!("╔════════════════════════════════════════════════════════════════════════════╗");
    log::info!("║                        MEMORY LAYOUT SUMMARY                               ║");
    log::info!("╠════════════════════════════════════════════════════════════════════════════╣");
    log::info!("║ Region              Virtual Address Range          Size        Purpose     ║");
    log::info!("╠════════════════════════════════════════════════════════════════════════════╣");

    // User space
    log::info!("║ User Space          0x0000000000000000            ~2 GB       Userspace    ║");
    log::info!("║                     0x0000000080000000                        (per-proc)   ║");
    log::info!("╟────────────────────────────────────────────────────────────────────────────╢");

    // Physmap
    let physmap_size_mb = physmap::max_phys() / (1024 * 1024);
    log::info!(
        "║ Physmap             0xffff800000000000            {} MB      Phys RAM      ║",
        format!("{:4}", physmap_size_mb).as_str()
    );
    log::info!(
        "║                     0x{:016x}                                 direct map   ║",
        physmap::PHYS_MAP_BASE + physmap::max_phys()
    );
    log::info!("╟────────────────────────────────────────────────────────────────────────────╢");

    // Kernel heap
    log::info!("║ Kernel Heap         0xffffffffc0000000              8 MB      Dynamic      ║");
    log::info!("║                     0xffffffffc0800000                        allocation   ║");
    log::info!("╟────────────────────────────────────────────────────────────────────────────╢");

    // Framebuffer
    let fb_size_kb = unsafe { (bootboot.fb_scanline * bootboot.fb_height) / 1024 };
    log::info!(
        "║ Framebuffer         0x{:016x}        {} KB      Graphics         ║",
        fb_addr,
        format!("{:4}", fb_size_kb).as_str()
    );
    log::info!("╟────────────────────────────────────────────────────────────────────────────╢");

    // Kernel code/data
    let kernel_size_kb = (kernel_end - kernel_start) / 1024;
    log::info!(
        "║ Kernel Code/Data    0x{:016x}        {} KB      Kernel           ║",
        kernel_start,
        format!("{:4}", kernel_size_kb).as_str()
    );
    log::info!(
        "║                     0x{:016x}                        text+data    ║",
        kernel_end
    );
    log::info!("╟────────────────────────────────────────────────────────────────────────────╢");

    // Kernel stack (BOOTBOOT)
    log::info!("║ Kernel Stack        0xffffffffffffe000              8 KB      Stack       ║");
    log::info!("║                     0xffffffffffffffff                        (grows ↓)   ║");
    log::info!("╠════════════════════════════════════════════════════════════════════════════╣");
    log::info!(
        "║ Physical Memory: {} / {} MB used ({:.1}%)                                ║",
        format!("{:4}", used_mb).as_str(),
        format!("{:4}", total_mb).as_str(),
        (used_frames as f32 / total_frames as f32) * 100.0
    );
    log::info!(
        "║ Page Table Root: 0x{:016x}                                          ║",
        kernel_address_space().page_table_root.as_u64()
    );
    log::info!("╚════════════════════════════════════════════════════════════════════════════╝");
    log::info!("");
}
