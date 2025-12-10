/*
 * Paging and Virtual Memory Manager
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

/// For now we assume physical memory is identity-mapped for lower memory.
/// If you later use a higher-half direct map, change this.
const PHYSICAL_MEMORY_OFFSET: u64 = 0x0;

/// Global page table mapper wrapped in a Mutex for safety.
static MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);

/// Frame allocator adapter that bridges our allocator with the x86_64 crate.
struct FrameAllocAdapter;

unsafe impl FrameAllocator<Size4KiB> for FrameAllocAdapter {
    fn allocate_frame(&mut self) -> Option<X86PhysFrame> {
        phys::alloc_frame()
            .map(|f| X86PhysFrame::containing_address(PhysAddr::new(f.start_address())))
    }
}

/// Initialize the paging system
pub fn init() {
    log::info!("Initializing paging system...");

    let physical_memory_offset = VirtAddr::new(PHYSICAL_MEMORY_OFFSET);

    // Get current top-level page table frame from CR3
    let (frame, _) = Cr3::read();
    let phys = frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();

    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    // Create the mapper
    let mapper = unsafe { OffsetPageTable::new(&mut *page_table_ptr, physical_memory_offset) };

    let mut guard = MAPPER.lock();
    *guard = Some(mapper);

    log::info!("Paging system initialized");
}

/// Map a virtual page to a specific physical frame
pub fn map_page(
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageTableFlags,
) -> Result<(), MapToError<Size4KiB>> {
    let mut guard = MAPPER.lock();
    let mapper = guard
        .as_mut()
        .expect("MAPPER not initialized in paging::map_page");

    let mut frame_allocator = FrameAllocAdapter;

    let page = Page::<Size4KiB>::containing_address(virt);
    let frame = X86PhysFrame::containing_address(phys);

    unsafe {
        mapper
            .map_to(page, frame, flags, &mut frame_allocator)?
            .flush();
    }

    Ok(())
}

/// Unmap a virtual page and free its backing physical frame
pub fn unmap_page(virt: VirtAddr) -> Result<(), &'static str> {
    let mut guard = MAPPER.lock();
    let mapper = guard
        .as_mut()
        .expect("MAPPER not initialized in paging::unmap_page");

    let page = Page::<Size4KiB>::containing_address(virt);

    match mapper.unmap(page) {
        Ok((frame, flush)) => {
            flush.flush();
            // Free the physical frame in our allocator
            let phys_frame = PhysFrame::containing_address(frame.start_address().as_u64());
            phys::free_frame(phys_frame);
            Ok(())
        }
        Err(_) => Err("Failed to unmap page"),
    }
}

/// Map a range of virtual pages to newly allocated physical frames
pub fn map_range(
    start_virt: VirtAddr,
    size: u64,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let page_count = (size + 0xfff) / 0x1000; // Round up to page boundary

    for i in 0..page_count {
        let virt = start_virt + (i * 0x1000);

        // Allocate a physical frame
        let phys_frame = phys::alloc_frame().ok_or("Out of physical memory")?;
        let phys = PhysAddr::new(phys_frame.start_address());

        // Map the page
        map_page(virt, phys, flags).map_err(|_| "Failed to map page")?;
    }

    Ok(())
}

/// Unmap a range of virtual pages
pub fn unmap_range(start_virt: VirtAddr, size: u64) -> Result<(), &'static str> {
    let page_count = (size + 0xfff) / 0x1000;

    for i in 0..page_count {
        let virt = start_virt + (i * 0x1000);
        unmap_page(virt)?;
    }

    Ok(())
}
