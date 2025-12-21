/*
 * Paging and Virtual Memory Manager
 *
 * This module manages virtual memory through the x86_64 paging system.
 * It provides high-level interfaces for mapping virtual addresses to
 * physical frames and managing page table operations.
 *
 * DESIGN OVERVIEW:
 * - Uses x86_64 crate's OffsetPageTable for page table management
 * - Integrates with our physical frame allocator for backing storage
 * - Supports 4 KiB pages (standard x86_64 page size)
 * - Thread-safe operations via mutex protection
 *
 * MEMORY MODEL:
 * - Currently assumes identity mapping for low physical memory access
 * - Page tables themselves are accessed via physical memory offset
 * - Virtual addresses can be mapped to any available physical frame
 *
 * KEY OPERATIONS:
 * - map_page: Map single virtual page to specific physical frame
 * - map_range: Map contiguous virtual range to newly allocated frames
 * - unmap_page/unmap_range: Remove mappings and free backing frames
 *
 * INTEGRATION POINTS:
 * - Uses PhysFrame allocator for obtaining backing physical memory
 * - Provides memory for heap allocator initialization
 * - Supports future user space memory management
 *
 * SAFETY CONSIDERATIONS:
 * - All page table access is protected by mutex
 * - Physical frame allocation/deallocation is atomic
 * - TLB flushes are performed after mapping changes
 */

use crate::memory::{PhysFrame, phys};
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags,
        PhysFrame as X86PhysFrame, Size4KiB, mapper::MapToError,
    },
};

/// Physical memory offset for accessing page tables
///
/// BOOTBOOT provides identity mapping for physical RAM access:
/// - Physical address X is accessible at virtual address X
/// - This is set up by BOOTBOOT in the lower half (0x0 - 0x400000000)
///
/// We use offset = 0 to work with BOOTBOOT's existing identity mapping.
/// We supplement it by ensuring all allocatable memory regions are mapped.
const PHYSICAL_MEMORY_OFFSET: u64 = 0x0;

/// Global page table mapper instance
/// Wrapped in Mutex to ensure thread-safe access to page table operations
/// The Option allows for lazy initialization during kernel boot
static MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);

/// Adapter that bridges our frame allocator with the x86_64 crate's interface
///
/// The x86_64 crate expects a FrameAllocator trait implementation, but our
/// allocator has a different interface. This adapter converts between them.
///
/// IMPORTANT: Physical memory must be identity-mapped during init so that
/// page table frames can be accessed during manipulation. We cannot map
/// on-demand here because that would cause mutex deadlock (map_page locks
/// MAPPER, which then calls this allocator, which would try to lock MAPPER again).
struct FrameAllocAdapter;

/// Implementation of x86_64 crate's FrameAllocator trait
///
/// SAFETY: This implementation is safe because:
/// - It delegates to our thread-safe physical frame allocator
/// - Frame allocation is atomic and properly synchronized
/// - Returned frames are guaranteed to be unused
/// - Physical memory is identity-mapped during paging init
unsafe impl FrameAllocator<Size4KiB> for FrameAllocAdapter {
    fn allocate_frame(&mut self) -> Option<X86PhysFrame> {
        // Allocate physical frame
        let frame = phys::alloc_frame()?;
        let phys_addr = frame.start_address();

        // Convert to x86_64 crate's type
        // NOTE: We assume this frame is already identity-mapped from init()
        Some(X86PhysFrame::containing_address(PhysAddr::new(phys_addr)))
    }
}

/// Initialize the paging system
///
/// Sets up the page table mapper by reading the current page table from CR3
/// and creating an OffsetPageTable instance for managing virtual memory.
///
/// This function must be called during kernel initialization, after the
/// physical frame allocator is set up but before any virtual memory operations.
pub fn init() {
    log::info!("Initializing paging system...");

    // Calculate virtual address offset for accessing physical memory
    let physical_memory_offset = VirtAddr::new(PHYSICAL_MEMORY_OFFSET);

    // Read the current page table address from CR3 register
    // CR3 contains the physical address of the top-level page table (PML4)
    let (frame, _) = Cr3::read();
    let phys = frame.start_address();

    // Convert physical address to virtual address for page table access
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    // Create the page table mapper
    // SAFETY: We're using the current page table from CR3, which is valid
    // The physical memory offset allows us to access page table entries
    let mut mapper = unsafe { OffsetPageTable::new(&mut *page_table_ptr, physical_memory_offset) };

    // Store the mapper in our global static for later use
    let mut guard = MAPPER.lock();
    *guard = Some(mapper);

    log::info!("Paging system initialized");
}

/// Map unmapped low memory regions reported as free by BOOTBOOT
///
/// BOOTBOOT only identity-maps memory from (initrd_end) onwards. Memory below
/// initrd_ptr is reported as free in the memory map but not mapped. This function
/// creates identity mappings for those regions so we can use them.
///
/// This must be called after both phys::init_from_bootboot() and paging::init().
pub fn map_low_memory(bootboot_ptr: *const crate::bootboot::BOOTBOOT) {
    use crate::bootboot::{MMapEnt, MMAP_FREE};
    use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB};

    let bootboot_ref = unsafe { &*bootboot_ptr };
    let initrd_ptr = bootboot_ref.initrd_ptr;

    log::info!("Mapping unmapped low memory regions (below 0x{:x})...", initrd_ptr);

    // Get memory map
    let bb_size = bootboot_ref.size;
    let total_bytes = (bb_size as usize).saturating_sub(128);
    let mmap_entries = total_bytes / core::mem::size_of::<MMapEnt>();
    let mmap_base: *const MMapEnt = core::ptr::addr_of!(bootboot_ref.mmap);

    let mut guard = MAPPER.lock();
    let mapper = guard.as_mut().expect("Mapper not initialized");
    let mut frame_alloc = FrameAllocAdapter;

    let mut mapped_frames = 0;

    // Process each memory map entry
    for i in 0..mmap_entries {
        let entry = unsafe { &*mmap_base.add(i) };
        let region_ptr: u64 = entry.ptr;
        let raw_size: u64 = entry.size;
        let entry_type: u32 = (raw_size & 0xF) as u32;
        let region_size: u64 = raw_size & !0xF;

        if region_size == 0 {
            continue;
        }

        // Only process free regions below initrd
        if entry_type == MMAP_FREE && region_ptr < initrd_ptr {
            let start_addr = region_ptr;
            let end_addr = region_ptr + region_size;

            // Clamp to initrd boundary
            let map_end = end_addr.min(initrd_ptr);

            // Map each 4KB page in this region
            let start_page = start_addr / 4096;
            let end_page = (map_end - 1) / 4096;

            log::debug!("  Mapping region 0x{:x}-0x{:x} (frames {}-{})",
                       start_addr, map_end, start_page, end_page);

            for page_num in start_page..=end_page {
                let virt_addr = page_num * 4096;
                let phys_addr = virt_addr; // Identity mapping

                let page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(virt_addr));
                let frame = X86PhysFrame::containing_address(PhysAddr::new(phys_addr));
                let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

                // Map the page (ignore AlreadyMapped errors)
                unsafe {
                    if let Ok(_) = mapper.map_to(page, frame, flags, &mut frame_alloc) {
                        mapped_frames += 1;
                    }
                }
            }
        }
    }

    drop(guard);

    log::info!("Mapped {} frames ({} MB) of low memory",
               mapped_frames, (mapped_frames * 4) / 1024);
}

/// Map a virtual page to a specific physical frame
/// 
/// Creates a page table entry that maps the given virtual address to the
/// specified physical address with the provided access flags.
/// 
/// # Arguments
/// * `virt` - Virtual address to map (will be rounded down to page boundary)
/// * `phys` - Physical address to map to (will be rounded down to frame boundary)
/// * `flags` - Page table flags (present, writable, user-accessible, etc.)
/// 
/// # Returns
/// * `Ok(())` - Mapping successful
/// * `Err(MapToError)` - Mapping failed (page already mapped, etc.)
/// 
/// # Safety
/// The caller must ensure that:
/// - The physical frame is not already in use for other purposes
/// - The virtual address is not already mapped
/// - The flags are appropriate for the intended use
pub fn map_page(
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageTableFlags,
) -> Result<(), MapToError<Size4KiB>> {
    // Acquire exclusive access to the page table mapper
    let mut guard = MAPPER.lock();
    let mapper = guard
        .as_mut()
        .expect("MAPPER not initialized in paging::map_page");

    // Create frame allocator for intermediate page table allocation
    let mut frame_allocator = FrameAllocAdapter;

    // Convert addresses to page/frame objects
    let page = Page::<Size4KiB>::containing_address(virt);
    let frame = X86PhysFrame::containing_address(phys);

    // Perform the mapping operation
    // SAFETY: We have exclusive access via mutex, and caller guarantees safety
    unsafe {
        mapper
            .map_to(page, frame, flags, &mut frame_allocator)?
            .flush(); // Flush TLB to ensure mapping is active
    }

    Ok(())
}

/// Map a page in a specific page table (by PhysAddr)
///
/// This function creates a temporary OffsetPageTable for the given page table root
/// and maps a page within it. This is used for mapping pages in userspace page tables
/// that are different from the currently active (kernel) page table.
///
/// # Arguments
/// * `page_table_root` - Physical address of the PML4 (page table root)
/// * `virt` - Virtual address to map
/// * `phys` - Physical address to map to
/// * `flags` - Page table flags
///
/// # Returns
/// * `Ok(())` - Mapping successful
/// * `Err(MapToError)` - Mapping failed
pub fn map_page_in_table(
    page_table_root: PhysAddr,
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageTableFlags,
) -> Result<(), MapToError<Size4KiB>> {
    use x86_64::structures::paging::OffsetPageTable;
    use x86_64::registers::control::Cr3;

    // CRITICAL: When accessing page tables (which may be in low memory),
    // we need to be in an address space that has entry 0 (identity mapping).
    // Syscalls run with userspace CR3, which doesn't have entry 0, causing
    // page faults when trying to access page table structures.
    //
    // Solution: Temporarily switch to kernel CR3 (PML4 at 0xd5d6000 or similar),
    // which has entry 0 and can access all physical memory.

    let (old_cr3_frame, old_cr3_flags) = Cr3::read();
    let old_cr3 = old_cr3_frame.start_address();

    // Get kernel CR3 (PID 0's page tables - the one BOOTBOOT set up)
    // We assume the kernel's CR3 is the one that has entry 0 for identity mapping
    // For now, only switch if we're NOT already in kernel CR3
    let kernel_cr3 = crate::scheduler::with_process_mut(
        crate::scheduler::ProcessId(0),
        |process| process.address_space.page_table_root
    ).unwrap_or(old_cr3);

    // Switch to kernel CR3 if needed
    let switched = old_cr3 != kernel_cr3;
    if switched {
        let kernel_frame = X86PhysFrame::containing_address(kernel_cr3);
        unsafe { Cr3::write(kernel_frame, old_cr3_flags); }
    }

    // Use the same high-half offset as the main mapper
    let physical_memory_offset = VirtAddr::new(PHYSICAL_MEMORY_OFFSET);

    // Convert physical address of page table to virtual address
    let page_table_virt = physical_memory_offset + page_table_root.as_u64();
    let page_table_ptr: *mut PageTable = page_table_virt.as_mut_ptr();

    // Create temporary mapper for this page table
    let mut mapper = unsafe { OffsetPageTable::new(&mut *page_table_ptr, physical_memory_offset) };
    let mut frame_allocator = FrameAllocAdapter;

    // Convert addresses to page/frame objects
    let page = Page::<Size4KiB>::containing_address(virt);
    let frame = X86PhysFrame::containing_address(phys);

    // Perform the mapping operation
    let result = unsafe {
        mapper
            .map_to(page, frame, flags, &mut frame_allocator)?
            .flush();
        Ok(())
    };

    // Switch back to original CR3 if we changed it
    if switched {
        let old_frame = X86PhysFrame::containing_address(old_cr3);
        unsafe { Cr3::write(old_frame, old_cr3_flags); }
    }

    result
}

/// Get the kernel's CR3 (PID 0's page table root)
///
/// This is safe to call from any context. Returns None if kernel process
/// doesn't exist (shouldn't happen after boot).
pub fn get_kernel_cr3() -> Option<PhysAddr> {
    crate::scheduler::with_process_mut(
        crate::scheduler::ProcessId(0),
        |process| process.address_space.page_table_root
    )
}

/// Map multiple pages in a batch operation (optimized for performance)
///
/// This function maps multiple virtual pages to physical frames in a single CR3 switch,
/// significantly improving performance when mapping many pages (e.g., user stack).
///
/// # Arguments
/// * `page_table_root` - Physical address of the target page table root
/// * `mappings` - Slice of (virtual_addr, physical_addr, flags) tuples to map
/// * `kernel_cr3` - Optional kernel CR3 to use for accessing page tables (if None, looks it up)
///
/// # Performance
/// Unlike map_page_in_table which switches CR3 for each page, this function:
/// - Gets kernel CR3 once
/// - Switches CR3 once before mapping
/// - Maps all pages
/// - Switches CR3 once after mapping
///
/// For 4096 pages, this reduces from 8192 CR3 switches to just 2!
///
/// # Returns
/// Ok(()) if all mappings succeed, or the first error encountered
pub fn map_pages_batch_in_table(
    page_table_root: PhysAddr,
    mappings: &[(VirtAddr, PhysAddr, PageTableFlags)],
    kernel_cr3: Option<PhysAddr>,
) -> Result<(), MapToError<Size4KiB>> {
    use x86_64::structures::paging::OffsetPageTable;
    use x86_64::registers::control::Cr3;

    // Early return if no mappings
    if mappings.is_empty() {
        return Ok(());
    }

    let (old_cr3_frame, old_cr3_flags) = Cr3::read();
    let old_cr3 = old_cr3_frame.start_address();

    // Get kernel CR3 - use provided value or look it up
    let kernel_cr3 = kernel_cr3.unwrap_or_else(|| {
        crate::scheduler::with_process_mut(
            crate::scheduler::ProcessId(0),
            |process| process.address_space.page_table_root
        ).unwrap_or(old_cr3)
    });

    // Switch to kernel CR3 once before mapping all pages
    let switched = old_cr3 != kernel_cr3;
    if switched {
        let kernel_frame = X86PhysFrame::containing_address(kernel_cr3);
        unsafe { Cr3::write(kernel_frame, old_cr3_flags); }
    }

    // Set up mapper once
    let physical_memory_offset = VirtAddr::new(PHYSICAL_MEMORY_OFFSET);
    let page_table_virt = physical_memory_offset + page_table_root.as_u64();
    let page_table_ptr: *mut PageTable = page_table_virt.as_mut_ptr();
    let mut mapper = unsafe { OffsetPageTable::new(&mut *page_table_ptr, physical_memory_offset) };
    let mut frame_allocator = FrameAllocAdapter;

    // Map all pages in the batch
    let mut result = Ok(());
    for (virt, phys, flags) in mappings {
        let page = Page::<Size4KiB>::containing_address(*virt);
        let frame = X86PhysFrame::containing_address(*phys);

        if let Err(e) = unsafe {
            mapper
                .map_to(page, frame, *flags, &mut frame_allocator)?
                .flush();
            Ok::<(), MapToError<Size4KiB>>(())
        } {
            result = Err(e);
            break;
        }
    }

    // Switch back to original CR3 once after mapping all pages
    if switched {
        let old_frame = X86PhysFrame::containing_address(old_cr3);
        unsafe { Cr3::write(old_frame, old_cr3_flags); }
    }

    result
}

/// Map a user-accessible page
///
/// This is a specialized version of map_page that ensures the USER_ACCESSIBLE
/// flag is set, allowing Ring 3 (userspace) code to access the page.
///
/// This function is used for mapping userspace memory regions (text, data,
/// heap, stack) and ensures proper privilege separation.
///
/// # Arguments
/// * `virt` - Virtual address to map (will be rounded down to page boundary)
/// * `phys` - Physical address to map to (will be rounded down to frame boundary)
/// * `flags` - Base page table flags (USER_ACCESSIBLE will be added automatically)
///
/// # Returns
/// * `Ok(())` - Mapping successful
/// * `Err(MapToError)` - Mapping failed (page already mapped, etc.)
///
/// # Safety
/// The caller must ensure that:
/// - The physical frame is not already in use
/// - The virtual address is in userspace range (< 0x0000_8000_0000_0000)
/// - The flags are appropriate (typically PRESENT | WRITABLE for data/heap/stack,
///   or PRESENT without WRITABLE for read-only code)
pub fn map_user_page(
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageTableFlags,
) -> Result<(), MapToError<Size4KiB>> {
    // Ensure USER_ACCESSIBLE is set for userspace access
    let user_flags = flags | PageTableFlags::USER_ACCESSIBLE;

    // Use the regular map_page with user flags
    map_page(virt, phys, user_flags)
}

/// Unmap a virtual page and free its backing physical frame
/// 
/// Removes the page table entry for the given virtual address and returns
/// the backing physical frame to the frame allocator for reuse.
/// 
/// # Arguments
/// * `virt` - Virtual address to unmap (will be rounded down to page boundary)
/// 
/// # Returns
/// * `Ok(())` - Unmapping successful, frame freed
/// * `Err(&str)` - Unmapping failed (page not mapped, etc.)
/// 
/// # Safety
/// The caller must ensure that:
/// - No code is currently using the virtual address being unmapped
/// - All references to data in the page have been dropped
/// - The page is not part of critical kernel structures
pub fn unmap_page(virt: VirtAddr) -> Result<(), &'static str> {
    // Acquire exclusive access to the page table mapper
    let mut guard = MAPPER.lock();
    let mapper = guard
        .as_mut()
        .expect("MAPPER not initialized in paging::unmap_page");

    // Convert virtual address to page object
    let page = Page::<Size4KiB>::containing_address(virt);

    // Attempt to unmap the page
    match mapper.unmap(page) {
        Ok((frame, flush)) => {
            // Flush TLB to ensure mapping is removed from CPU caches
            flush.flush();
            
            // Return the physical frame to our allocator for reuse
            let phys_frame = PhysFrame::containing_address(frame.start_address().as_u64());
            phys::free_frame(phys_frame);
            Ok(())
        }
        Err(_) => Err("Failed to unmap page - page may not be mapped"),
    }
}

/// Map a range of virtual pages to newly allocated physical frames
/// 
/// Allocates physical frames and maps them to a contiguous virtual address
/// range. This is commonly used for setting up heap regions, stacks, etc.
/// 
/// # Arguments
/// * `start_virt` - Starting virtual address of the range
/// * `size` - Size of the range in bytes (will be rounded up to page boundary)
/// * `flags` - Page table flags to apply to all pages in the range
/// 
/// # Returns
/// * `Ok(())` - All pages successfully mapped
/// * `Err(&str)` - Mapping failed (out of memory, mapping conflict, etc.)
/// 
/// # Behavior
/// If mapping fails partway through, already-mapped pages remain mapped.
/// The caller should call unmap_range to clean up on error.
pub fn map_range(
    start_virt: VirtAddr,
    size: u64,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    // Calculate number of pages needed (round up to page boundary)
    let page_count = (size + 0xfff) / 0x1000;

    // Map each page in the range
    for i in 0..page_count {
        // Calculate virtual address for this page
        let virt = start_virt + (i * 0x1000);

        // Allocate a fresh physical frame for this page
        let phys_frame = phys::alloc_frame().ok_or("Out of physical memory")?;
        let phys = PhysAddr::new(phys_frame.start_address());

        // Map the virtual page to the physical frame
        map_page(virt, phys, flags).map_err(|_| "Failed to map page in range")?;
    }

    Ok(())
}

/// Unmap a range of virtual pages and free their backing frames
/// 
/// Removes page table entries for a contiguous virtual address range and
/// returns all backing physical frames to the frame allocator.
/// 
/// # Arguments
/// * `start_virt` - Starting virtual address of the range to unmap
/// * `size` - Size of the range in bytes (will be rounded up to page boundary)
/// 
/// # Returns
/// * `Ok(())` - All pages successfully unmapped
/// * `Err(&str)` - Unmapping failed for at least one page
/// 
/// # Behavior
/// Continues unmapping even if individual pages fail, to clean up as much
/// as possible. Returns error if any page failed to unmap.
pub fn unmap_range(start_virt: VirtAddr, size: u64) -> Result<(), &'static str> {
    // Calculate number of pages to unmap (round up to page boundary)
    let page_count = (size + 0xfff) / 0x1000;

    // Track if any unmapping operations failed
    let mut any_failed = false;

    // Unmap each page in the range
    for i in 0..page_count {
        // Calculate virtual address for this page
        let virt = start_virt + (i * 0x1000);
        
        // Attempt to unmap this page, but continue even if it fails
        if unmap_page(virt).is_err() {
            any_failed = true;
        }
    }

    // Return error if any individual unmap failed
    if any_failed {
        Err("Failed to unmap one or more pages in range")
    } else {
        Ok(())
    }
}
