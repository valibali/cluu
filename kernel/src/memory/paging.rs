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
/// Currently assumes identity mapping (physical addr = virtual addr)
/// TODO: Update this when implementing higher-half kernel mapping
const PHYSICAL_MEMORY_OFFSET: u64 = 0x0;

/// Global page table mapper instance
/// Wrapped in Mutex to ensure thread-safe access to page table operations
/// The Option allows for lazy initialization during kernel boot
static MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);

/// Adapter that bridges our frame allocator with the x86_64 crate's interface
/// 
/// The x86_64 crate expects a FrameAllocator trait implementation, but our
/// allocator has a different interface. This adapter converts between them.
struct FrameAllocAdapter;

/// Implementation of x86_64 crate's FrameAllocator trait
/// 
/// SAFETY: This implementation is safe because:
/// - It delegates to our thread-safe physical frame allocator
/// - Frame allocation is atomic and properly synchronized
/// - Returned frames are guaranteed to be unused
unsafe impl FrameAllocator<Size4KiB> for FrameAllocAdapter {
    fn allocate_frame(&mut self) -> Option<X86PhysFrame> {
        // Call our allocator and convert the result to x86_64 crate's type
        phys::alloc_frame()
            .map(|f| X86PhysFrame::containing_address(PhysAddr::new(f.start_address())))
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
    let mapper = unsafe { OffsetPageTable::new(&mut *page_table_ptr, physical_memory_offset) };

    // Store the mapper in our global static for later use
    let mut guard = MAPPER.lock();
    *guard = Some(mapper);

    log::info!("Paging system initialized");
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

    const PHYSICAL_MEMORY_OFFSET: u64 = 0x0;
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
    unsafe {
        mapper
            .map_to(page, frame, flags, &mut frame_allocator)?
            .flush();
    }

    Ok(())
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
