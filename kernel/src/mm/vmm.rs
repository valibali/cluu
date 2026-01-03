//! Virtual Memory Manager
//!
//! This module provides page table management using the x86_64 crate's
//! `OffsetPageTable` abstraction, integrated with our BuddyAllocator for
//! frame allocation.
//!
//! # Architecture
//!
//! - **PageTableManager**: Wraps `OffsetPageTable` and provides map/unmap operations
//! - **FrameAllocatorAdapter**: Adapts our `PageAllocator` trait to x86_64's `FrameAllocator`
//! - **Physmap Integration**: Uses physmap offset for page table access
//!
//! # Design Principles
//!
//! Following SOLID principles from Phase 2:
//! - Single Responsibility: VMM only handles virtual→physical mapping
//! - Dependency Inversion: Depends on `PageAllocator` trait, not concrete types
//! - Interface Segregation: Clean separation from AddressSpace management
//!
//! # Example Usage
//!
//! ```rust,no_run
//! use crate::mm::{BuddyAllocator, PageTableManager, PageFlags};
//! use x86_64::VirtAddr;
//!
//! // Create page table manager with buddy allocator
//! let mut allocator = BuddyAllocator::new(&regions);
//! let mut vmm = unsafe {
//!     PageTableManager::new(
//!         VirtAddr::new(0xffff_8000_0000_0000), // Physmap base
//!         &mut allocator
//!     )
//! };
//!
//! // Map a page
//! vmm.map(
//!     VirtAddr::new(0x400000),
//!     PageFlags::new().present().writable().user()
//! )?;
//! ```

use crate::mm::traits::{
    MapError, PageAllocator, PageFlags, UnmapError, VirtualMemoryMapper,
};
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, Mapper, OffsetPageTable, Page, PageTable,
    PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

/// Page Table Manager
///
/// Wraps x86_64's `OffsetPageTable` and integrates with our memory allocator.
/// Provides high-level operations for virtual memory management:
/// - Map virtual pages to physical frames
/// - Unmap pages and free underlying frames
/// - Change page protections
/// - Translate virtual to physical addresses
///
/// # Safety Invariants
///
/// - Must be created with valid physmap offset
/// - Frame allocator must provide properly aligned 4KB frames
/// - CR3 must point to valid page table when operations are performed
pub struct PageTableManager<'a, A: PageAllocator> {
    /// x86_64 crate's page table mapper
    mapper: OffsetPageTable<'static>,

    /// Frame allocator adapter
    frame_allocator: FrameAllocatorAdapter<'a, A>,
}

impl<'a, A: PageAllocator> PageTableManager<'a, A> {
    /// Create a new page table manager
    ///
    /// # Arguments
    ///
    /// * `phys_offset` - Virtual address of physmap base (typically 0xffff_8000_0000_0000)
    /// * `allocator` - Physical memory allocator implementing `PageAllocator` trait
    ///
    /// # Safety
    ///
    /// - `phys_offset` must be the correct physmap offset where physical memory is mapped
    /// - The current CR3 must point to a valid page table
    /// - The page table must be accessible via physmap
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// let mut buddy = BuddyAllocator::new(&regions);
    /// let vmm = unsafe {
    ///     PageTableManager::new(
    ///         VirtAddr::new(0xffff_8000_0000_0000),
    ///         &mut buddy
    ///     )
    /// };
    /// ```
    pub unsafe fn new(phys_offset: VirtAddr, allocator: &'a mut A) -> Self {
        let level_4_table = unsafe { active_level_4_table(phys_offset) };
        let mapper = unsafe { OffsetPageTable::new(level_4_table, phys_offset) };

        Self {
            mapper,
            frame_allocator: FrameAllocatorAdapter::new(allocator),
        }
    }

    /// Create page table manager for a specific page table root
    ///
    /// Unlike `new()` which uses the active page table (current CR3),
    /// this creates a manager for an arbitrary page table.
    ///
    /// # Arguments
    ///
    /// * `phys_offset` - Virtual address of physmap base
    /// * `page_table_root` - Physical address of PML4 (page table root)
    /// * `allocator` - Physical memory allocator
    ///
    /// # Safety
    ///
    /// - `page_table_root` must point to a valid PML4 table
    /// - The PML4 must be accessible via physmap
    /// - Physmap must be properly set up at `phys_offset`
    pub unsafe fn for_page_table(
        phys_offset: VirtAddr,
        page_table_root: PhysAddr,
        allocator: &'a mut A,
    ) -> Self {
        // Calculate virtual address of page table via physmap
        let page_table_virt = phys_offset + page_table_root.as_u64();
        let level_4_table = unsafe { &mut *(page_table_virt.as_mut_ptr::<PageTable>()) };
        let mapper = unsafe { OffsetPageTable::new(level_4_table, phys_offset) };

        Self {
            mapper,
            frame_allocator: FrameAllocatorAdapter::new(allocator),
        }
    }

    /// Map a virtual page with lazy allocation
    ///
    /// Allocates a physical frame and maps it to the virtual address.
    /// Creates intermediate page tables as needed.
    ///
    /// # Arguments
    ///
    /// * `virt` - Virtual address to map (will be page-aligned)
    /// * `flags` - Page flags (present, writable, user, no_execute, etc.)
    ///
    /// # Returns
    ///
    /// * `Ok(PhysAddr)` - Physical address of the mapped frame
    /// * `Err(MapError)` - If mapping fails (page already mapped, OOM, etc.)
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// let phys = vmm.map(
    ///     VirtAddr::new(0x400000),
    ///     PageFlags::new().present().writable().user()
    /// )?;
    /// ```
    pub fn map(&mut self, virt: VirtAddr, flags: PageFlags) -> Result<PhysAddr, MapError> {
        let page: Page<Size4KiB> = Page::containing_address(virt);
        let x86_flags = convert_flags_to_x86(flags);

        // Allocate a physical frame
        let frame = self
            .frame_allocator
            .allocate_frame()
            .ok_or(MapError::OutOfMemory)?;

        // Map page to frame
        unsafe {
            self.mapper
                .map_to(page, frame, x86_flags, &mut self.frame_allocator)
                .map_err(|e| match e {
                    x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed => {
                        MapError::OutOfMemory
                    }
                    x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_) => {
                        MapError::AlreadyMapped
                    }
                    x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage => {
                        MapError::InvalidAddress
                    }
                })?
                .flush();
        }

        Ok(frame.start_address())
    }

    /// Map a virtual page to a specific physical frame
    ///
    /// Similar to `map()` but uses a caller-provided physical frame instead
    /// of allocating one. Useful for:
    /// - Mapping MMIO regions
    /// - Sharing memory between address spaces
    /// - Implementing grant/map IPC operations
    ///
    /// # Arguments
    ///
    /// * `virt` - Virtual address to map
    /// * `phys` - Physical address of frame to map
    /// * `flags` - Page flags
    ///
    /// # Safety
    ///
    /// - `phys` must point to a valid physical frame
    /// - Caller must ensure the frame is not freed while mapped
    /// - For shared mappings, caller must coordinate access
    pub unsafe fn map_to(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
    ) -> Result<(), MapError> {
        let page: Page<Size4KiB> = Page::containing_address(virt);
        let frame = PhysFrame::containing_address(phys);
        let x86_flags = convert_flags_to_x86(flags);

        unsafe {
            self.mapper
                .map_to(page, frame, x86_flags, &mut self.frame_allocator)
                .map_err(|e| match e {
                    x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed => {
                        MapError::OutOfMemory
                    }
                    x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_) => {
                        MapError::AlreadyMapped
                    }
                    x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage => {
                        MapError::InvalidAddress
                    }
                })?
                .flush();
        }

        Ok(())
    }

    /// Unmap a virtual page
    ///
    /// Removes the mapping and returns the physical address that was mapped.
    /// The underlying physical frame is NOT freed - caller must free it if needed.
    ///
    /// # Arguments
    ///
    /// * `virt` - Virtual address to unmap
    ///
    /// # Returns
    ///
    /// * `Ok(PhysAddr)` - Physical address that was unmapped
    /// * `Err(UnmapError)` - If page was not mapped
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// let phys = vmm.unmap(VirtAddr::new(0x400000))?;
    /// // Optionally free the frame
    /// allocator.free(phys, 0);
    /// ```
    pub fn unmap(&mut self, virt: VirtAddr) -> Result<PhysAddr, UnmapError> {
        let page: Page<Size4KiB> = Page::containing_address(virt);

        let (frame, flush) = self
            .mapper
            .unmap(page)
            .map_err(|_| UnmapError::NotMapped)?;

        flush.flush();

        Ok(frame.start_address())
    }

    /// Change protection flags on a mapped page
    ///
    /// Updates the page table flags without changing the physical mapping.
    /// Useful for implementing mprotect-style operations.
    ///
    /// # Arguments
    ///
    /// * `virt` - Virtual address of page to modify
    /// * `flags` - New page flags
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Protection updated successfully
    /// * `Err(MapError)` - If page is not mapped
    pub fn protect(&mut self, virt: VirtAddr, flags: PageFlags) -> Result<(), MapError> {
        let page: Page<Size4KiB> = Page::containing_address(virt);
        let x86_flags = convert_flags_to_x86(flags);

        // Update flags and flush TLB
        unsafe {
            self.mapper
                .update_flags(page, x86_flags)
                .map_err(|_| MapError::InvalidAddress)?
                .flush();
        }

        Ok(())
    }

    /// Translate virtual address to physical address
    ///
    /// Walks the page tables to find the physical address corresponding
    /// to a virtual address.
    ///
    /// # Arguments
    ///
    /// * `virt` - Virtual address to translate
    ///
    /// # Returns
    ///
    /// * `Some(PhysAddr)` - Physical address if page is mapped
    /// * `None` - If page is not mapped
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// if let Some(phys) = vmm.translate(VirtAddr::new(0x400000)) {
    ///     println!("Virtual 0x400000 maps to physical {:?}", phys);
    /// }
    /// ```
    pub fn translate(&self, virt: VirtAddr) -> Option<PhysAddr> {
        use x86_64::structures::paging::mapper::Translate;

        match self.mapper.translate(virt) {
            x86_64::structures::paging::mapper::TranslateResult::Mapped {
                frame,
                offset,
                flags: _,
            } => Some(frame.start_address() + offset),
            _ => None,
        }
    }

    /// Get the physical address of the page table root (PML4)
    ///
    /// Returns the physical address that would be loaded into CR3
    /// for this page table.
    pub fn page_table_root(&self) -> PhysAddr {
        // Get the virtual address of the level 4 table
        let level_4_virt = self.mapper.level_4_table() as *const _ as u64;

        // Calculate physical address using physmap offset
        // physmap maps physical [0..max) to virtual [phys_offset..phys_offset+max)
        // So: phys = virt - phys_offset
        // We need to get the physmap offset. For now, use the known constant.
        // In production, this should be passed in or stored.
        let phys_offset = 0xffff_8000_0000_0000u64;

        PhysAddr::new(level_4_virt - phys_offset)
    }
}

impl<'a, A: PageAllocator> VirtualMemoryMapper for PageTableManager<'a, A> {
    fn map(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
    ) -> Result<(), MapError> {
        unsafe { self.map_to(virt, phys, flags) }
    }

    fn unmap(&mut self, virt: VirtAddr) -> Result<PhysAddr, UnmapError> {
        self.unmap(virt)
    }

    fn protect(&mut self, virt: VirtAddr, flags: PageFlags) -> Result<(), MapError> {
        self.protect(virt, flags)
    }

    fn translate(&self, virt: VirtAddr) -> Option<PhysAddr> {
        self.translate(virt)
    }
}

/// Frame allocator adapter
///
/// Adapts our `PageAllocator` trait to the x86_64 crate's `FrameAllocator` trait.
/// This allows us to use the BuddyAllocator with x86_64's page table operations.
struct FrameAllocatorAdapter<'a, A: PageAllocator> {
    allocator: &'a mut A,
}

impl<'a, A: PageAllocator> FrameAllocatorAdapter<'a, A> {
    fn new(allocator: &'a mut A) -> Self {
        Self { allocator }
    }
}

unsafe impl<'a, A: PageAllocator> FrameAllocator<Size4KiB> for FrameAllocatorAdapter<'a, A> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        // Allocate a single page (order 0) from our allocator
        let phys_addr = self.allocator.alloc(0)?;
        Some(PhysFrame::containing_address(phys_addr))
    }
}

impl<'a, A: PageAllocator> FrameDeallocator<Size4KiB> for FrameAllocatorAdapter<'a, A> {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        // Free the frame back to our allocator (order 0)
        self.allocator.free(frame.start_address(), 0);
    }
}

/// Get a reference to the active level 4 page table
///
/// # Safety
///
/// - `phys_offset` must be the correct physmap offset
/// - Current CR3 must point to a valid page table
/// - Page table must be accessible via physmap
unsafe fn active_level_4_table(phys_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;

    let (frame, _) = Cr3::read();
    let phys = frame.start_address();
    let virt = phys_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    unsafe { &mut *page_table_ptr }
}

/// Convert our PageFlags to x86_64's PageTableFlags
fn convert_flags_to_x86(flags: PageFlags) -> PageTableFlags {
    let mut x86_flags = PageTableFlags::empty();

    if flags.present {
        x86_flags |= PageTableFlags::PRESENT;
    }
    if flags.writable {
        x86_flags |= PageTableFlags::WRITABLE;
    }
    if flags.user {
        x86_flags |= PageTableFlags::USER_ACCESSIBLE;
    }
    if flags.no_execute {
        x86_flags |= PageTableFlags::NO_EXECUTE;
    }
    if flags.write_through {
        x86_flags |= PageTableFlags::WRITE_THROUGH;
    }
    if flags.cache_disabled {
        x86_flags |= PageTableFlags::NO_CACHE;
    }
    if flags.accessed {
        x86_flags |= PageTableFlags::ACCESSED;
    }
    if flags.dirty {
        x86_flags |= PageTableFlags::DIRTY;
    }
    if flags.huge {
        x86_flags |= PageTableFlags::HUGE_PAGE;
    }
    if flags.global {
        x86_flags |= PageTableFlags::GLOBAL;
    }

    x86_flags
}

/// Convert x86_64's PageTableFlags to our PageFlags
#[allow(dead_code)]
fn convert_flags_from_x86(x86_flags: PageTableFlags) -> PageFlags {
    PageFlags {
        present: x86_flags.contains(PageTableFlags::PRESENT),
        writable: x86_flags.contains(PageTableFlags::WRITABLE),
        user: x86_flags.contains(PageTableFlags::USER_ACCESSIBLE),
        no_execute: x86_flags.contains(PageTableFlags::NO_EXECUTE),
        write_through: x86_flags.contains(PageTableFlags::WRITE_THROUGH),
        cache_disabled: x86_flags.contains(PageTableFlags::NO_CACHE),
        accessed: x86_flags.contains(PageTableFlags::ACCESSED),
        dirty: x86_flags.contains(PageTableFlags::DIRTY),
        huge: x86_flags.contains(PageTableFlags::HUGE_PAGE),
        global: x86_flags.contains(PageTableFlags::GLOBAL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::{BuddyAllocator, MemoryRegion, MockPageAllocator};

    const PAGE_SIZE: u64 = 4096;

    /// Test flag conversion: our flags → x86_64 flags
    #[test]
    fn test_convert_flags_to_x86() {
        let flags = PageFlags::user(); // present, writable, user-accessible
        let x86_flags = convert_flags_to_x86(flags);

        assert!(x86_flags.contains(PageTableFlags::PRESENT));
        assert!(x86_flags.contains(PageTableFlags::WRITABLE));
        assert!(x86_flags.contains(PageTableFlags::USER_ACCESSIBLE));
        assert!(!x86_flags.contains(PageTableFlags::NO_EXECUTE));
    }

    /// Test flag conversion: x86_64 flags → our flags
    #[test]
    fn test_convert_flags_from_x86() {
        let x86_flags =
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        let flags = convert_flags_from_x86(x86_flags);

        assert!(flags.present);
        assert!(flags.writable);
        assert!(!flags.user);
        assert!(flags.no_execute);
    }

    /// Test flag round-trip conversion
    #[test]
    fn test_flag_conversion_round_trip() {
        let original = PageFlags {
            present: true,
            writable: true,
            user: true,
            no_execute: true,
            write_through: false,
            cache_disabled: false,
            accessed: true,
            dirty: true,
            huge: false,
            global: false,
        };

        let x86_flags = convert_flags_to_x86(original);
        let converted_back = convert_flags_from_x86(x86_flags);

        assert_eq!(original.present, converted_back.present);
        assert_eq!(original.writable, converted_back.writable);
        assert_eq!(original.user, converted_back.user);
        assert_eq!(original.no_execute, converted_back.no_execute);
        assert_eq!(original.accessed, converted_back.accessed);
        assert_eq!(original.dirty, converted_back.dirty);
    }

    /// Test frame allocator adapter with mock allocator
    #[test]
    fn test_frame_allocator_adapter() {
        let mut mock = MockPageAllocator::new();
        let mut adapter = FrameAllocatorAdapter::new(&mut mock);

        // Test allocation
        let frame1 = adapter.allocate_frame();
        assert!(frame1.is_some());

        let frame2 = adapter.allocate_frame();
        assert!(frame2.is_some());

        // Frames should be different
        assert_ne!(
            frame1.unwrap().start_address(),
            frame2.unwrap().start_address()
        );
    }

    /// Test frame allocator adapter handles OOM
    #[test]
    fn test_frame_allocator_adapter_oom() {
        let mut mock = MockPageAllocator::new();
        mock.set_should_fail(true);

        let mut adapter = FrameAllocatorAdapter::new(&mut mock);

        // Should return None when allocator fails
        let frame = adapter.allocate_frame();
        assert!(frame.is_none());
    }

    /// Test frame deallocation
    #[test]
    fn test_frame_deallocator() {
        let mut mock = MockPageAllocator::new();

        // Track initial allocation count
        let initial_count = mock.allocation_count();
        assert_eq!(initial_count, 0);

        // Allocate and deallocate in a block to allow borrowing mock after
        {
            let mut adapter = FrameAllocatorAdapter::new(&mut mock);

            // Allocate a frame
            let frame = adapter.allocate_frame().expect("allocation failed");

            // Deallocate the frame
            unsafe {
                adapter.deallocate_frame(frame);
            }
        }

        // After deallocation, count should be back to initial
        assert_eq!(mock.allocation_count(), initial_count);
    }
}
