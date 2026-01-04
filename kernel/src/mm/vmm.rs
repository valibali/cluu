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

use crate::mm::traits::{MapError, PageAllocator, PageFlags, UnmapError, VirtualMemoryMapper};
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags,
    PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

// ═══════════════════════════════════════════════════════════════════════════
// Page Table Entry Flags (for manual page table manipulation)
// ═══════════════════════════════════════════════════════════════════════════

/// Page table entry flag constants
/// These are bit flags used in page table entries (PML4E, PDPTE, PDE, PTE)
mod pte_flags {
    /// Page is present in memory (bit 0)
    pub const PRESENT: u64 = 1 << 0;

    /// Page is writable (bit 1)
    pub const WRITABLE: u64 = 1 << 1;

    /// User-accessible (bit 2)
    pub const USER: u64 = 1 << 2;

    /// Huge page / Page size (bit 7)
    /// In PDE: 1 = 2MB page, 0 = points to PT
    pub const HUGE: u64 = 1 << 7;

    /// No execute (bit 63) - requires NX support
    pub const NO_EXECUTE: u64 = 1 << 63;
}

// Common flag combinations for convenience
const PTE_PRESENT_WRITABLE: u64 = pte_flags::PRESENT | pte_flags::WRITABLE;
const PTE_HUGE_PAGE: u64 = pte_flags::PRESENT | pte_flags::WRITABLE | pte_flags::HUGE;

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

        let (frame, flush) = self.mapper.unmap(page).map_err(|_| UnmapError::NotMapped)?;

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
    fn map(&mut self, virt: VirtAddr, phys: PhysAddr, flags: PageFlags) -> Result<(), MapError> {
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

// ═══════════════════════════════════════════════════════════════════════════
// Initial Page Table Creation
// ═══════════════════════════════════════════════════════════════════════════

/// Create initial kernel page tables with physmap
///
/// This function creates new page tables from scratch with:
/// - Kernel mapped at high address (preserve BOOTBOOT mapping)
/// - Physmap: All physical RAM at 0xffff_8000_0000_0000
///
/// **Hybrid Page Size Strategy:**
/// - 2MB huge pages for aligned bulk RAM (performance)
/// - 4KB pages for unaligned regions, edges, and MMIO (correctness)
///
/// # Arguments
///
/// * `max_phys` - Maximum physical address to map in physmap
/// * `kernel_phys_start` - Physical address of kernel start
/// * `kernel_phys_end` - Physical address of kernel end
///
/// # Returns
///
/// Physical address of PML4 (ready to load into CR3)
///
/// # Safety
///
/// Must be called during bootstrap before switching page tables.
/// Uses BOOTBOOT identity mapping to access allocated pages.
pub unsafe fn create_initial_page_tables(
    boot_info: &dyn crate::mm::boot::BootInfoProvider,
    max_phys: u64,
    kernel_phys_start: u64,
    kernel_phys_end: u64,
) -> PhysAddr {
    use crate::mm::physmap::PHYS_MAP_BASE;
    use core::ptr::write_bytes;

    // Linker symbols for kernel virtual base
    extern "C" {
        static __text_start: u8;
    }

    // Step 1: Initialize PMM (Physical Memory Manager)
    // Get BOOTBOOT pointer to parse memory map
    extern "C" {
        static bootboot: crate::bootboot::BOOTBOOT;
    }
    let bootboot_ptr = unsafe { &bootboot as *const _ };
    unsafe {
        crate::mm::pmm_simple::init(&*bootboot_ptr, boot_info);
    }

    klibcluu::info("Creating initial page tables (4KB pages only)...");
    klibcluu::log_hex(
        klibcluu::LogLevel::Trace,
        "  Max physical address: 0x",
        max_phys,
    );
    klibcluu::log_hex(
        klibcluu::LogLevel::Trace,
        "  Kernel phys range: 0x",
        kernel_phys_start,
    );
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "    to 0x", kernel_phys_end);

    // Get kernel virtual base from linker symbol and align to page boundary
    let kernel_virt_base_unaligned = unsafe { &__text_start as *const u8 as u64 };
    let kernel_virt_base = kernel_virt_base_unaligned & !0xFFF; // Round down to page
    let kernel_phys_start_aligned = kernel_phys_start & !0xFFF; // Round down to page

    // Step 2: Allocate PML4 from PMM (returns physical address)
    let pml4_phys_addr = crate::mm::pmm_simple::alloc_frame().expect("Failed to allocate PML4");
    let pml4_phys = PhysAddr::new(pml4_phys_addr);

    // Access PML4 through BOOTBOOT's identity mapping (physical address == virtual address for identity-mapped regions)
    let pml4 = unsafe { &mut *(pml4_phys_addr as *mut [u64; 512]) };
    unsafe { write_bytes(pml4.as_mut_ptr(), 0, 4096) };

    // Map physmap region (0xffff_8000_0000_0000)
    let physmap_pml4_idx = ((PHYS_MAP_BASE >> 39) & 0x1FF) as usize;

    // Allocate PDPT for physmap from PMM
    let physmap_pdpt_phys =
        crate::mm::pmm_simple::alloc_frame().expect("Failed to allocate PDPT for physmap");
    let physmap_pdpt = unsafe { &mut *(physmap_pdpt_phys as *mut [u64; 512]) };
    unsafe { write_bytes(physmap_pdpt.as_mut_ptr(), 0, 4096) };
    pml4[physmap_pml4_idx] = physmap_pdpt_phys | 0x3;

    // Map physmap using hybrid approach
    // Use 2MB pages for aligned regions, 4KB for edges
    let mut total_2mb_pages = 0;
    let mut total_4kb_pages = 0;

    // Calculate aligned 2MB region
    let aligned_end = max_phys & !(0x1F_FFFF); // Round down to 2MB
    let partial_start = aligned_end;
    let partial_size = max_phys - aligned_end;

    klibcluu::log_hex(
        klibcluu::LogLevel::Debug,
        "  Aligned region: 0x0 - 0x",
        aligned_end,
    );
    if partial_size > 0 {
        klibcluu::log_hex(
            klibcluu::LogLevel::Debug,
            "  Partial region: 0x",
            partial_start,
        );
        klibcluu::log_hex(klibcluu::LogLevel::Debug, "    to 0x", max_phys);
    }

    // Map aligned region with 2MB pages
    if aligned_end > 0 {
        let gb_count = ((aligned_end + 0x3FFF_FFFF) / 0x4000_0000) as usize;

        for gb_idx in 0..gb_count.min(512) {
            let pd_phys = crate::mm::pmm_simple::alloc_frame().expect("Failed to allocate PD");
            let pd = unsafe { &mut *(pd_phys as *mut [u64; 512]) };
            unsafe { write_bytes(pd.as_mut_ptr(), 0, 4096) };
            physmap_pdpt[gb_idx] = pd_phys | 0x3;
            let base_phys = (gb_idx as u64) * 0x4000_0000;

            for pd_idx in 0..512 {
                let phys_addr = base_phys + (pd_idx as u64) * 0x20_0000;
                if phys_addr < aligned_end {
                    // 2MB huge page: Present | Writable | Huge Page (0x80)
                    pd[pd_idx] = phys_addr | 0x83;
                    total_2mb_pages += 1;
                }
            }
        }
    }

    // Map partial region with 4KB pages if needed
    if partial_size > 0 {
        unsafe {
            map_4kb_region(
                physmap_pdpt,
                partial_start,
                max_phys,
                &mut total_4kb_pages,
                kernel_virt_base,
                kernel_phys_start,
            );
        }
    }

    klibcluu::info("  Physmap mapped:");
    klibcluu::log_dec(
        klibcluu::LogLevel::Trace,
        "    2MB pages: ",
        total_2mb_pages,
    );
    klibcluu::log_dec(
        klibcluu::LogLevel::Trace,
        "    (",
        (total_2mb_pages * 2) / 1024,
    );
    klibcluu::info(" GB)");
    if total_4kb_pages > 0 {
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "    4KB pages: ",
            total_4kb_pages,
        );
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "    (",
            (total_4kb_pages * 4) / 1024,
        );
        klibcluu::info(" MB)");
    }

    // Map kernel at high address using 4KB pages
    // We MUST use 4KB pages because the virtual offset != physical offset
    // (e.g., virtual 0xffffffffffe02078 has offset 0x2078, but physical 0xe000078 has offset 0x78)
    let kernel_pml4_idx = 511;
    let kernel_pdpt_phys =
        crate::mm::pmm_simple::alloc_frame().expect("Failed to allocate kernel PDPT");
    let kernel_pdpt = unsafe { &mut *(kernel_pdpt_phys as *mut [u64; 512]) };
    unsafe { write_bytes(kernel_pdpt.as_mut_ptr(), 0, 4096) };
    pml4[kernel_pml4_idx] = kernel_pdpt_phys | 0x3;

    let kernel_pd_phys =
        crate::mm::pmm_simple::alloc_frame().expect("Failed to allocate kernel PD");
    let kernel_pd = unsafe { &mut *(kernel_pd_phys as *mut [u64; 512]) };
    unsafe { write_bytes(kernel_pd.as_mut_ptr(), 0, 4096) };
    kernel_pdpt[511] = kernel_pd_phys | 0x3;

    // Map kernel using 4KB pages
    // Kernel virtual range: [kernel_virt_base, kernel_virt_base + size)
    // Kernel physical range: [kernel_phys_start, kernel_phys_end)
    // Mapping: virt_page → phys_page where phys_page = kernel_phys_start + (virt_page - kernel_virt_base)

    let kernel_size = kernel_phys_end - kernel_phys_start;
    let kernel_virt_end = kernel_virt_base.wrapping_add(kernel_size);

    // Calculate how many 2MB blocks we need (avoid wrapping arithmetic in loop condition!)
    let kernel_size_rounded = (kernel_size + 0x1F_FFFF) & !(0x1F_FFFF); // Round up to 2MB
    let num_2mb_blocks = (kernel_size_rounded / 0x20_0000) as usize;

    klibcluu::log_dec(
        klibcluu::LogLevel::Trace,
        "  Mapping kernel (",
        num_2mb_blocks as u64,
    );
    klibcluu::info(" x 2MB blocks)");

    // For each 2MB block (use counter instead of address comparison to avoid wrapping issues)
    let virt_start_2mb = kernel_virt_base & !(0x1F_FFFF);
    let mut first_pt_slice: Option<&[u64]> = None;
    for block_idx in 0..num_2mb_blocks {
        let virt_2mb = virt_start_2mb.wrapping_add(block_idx as u64 * 0x20_0000);
        // Calculate PD index from virtual address
        let pd_idx = ((virt_2mb >> 21) & 0x1FF) as usize;

        // Allocate PT for this 2MB block from PMM
        let pt_phys = crate::mm::pmm_simple::alloc_frame().expect("Failed to allocate kernel PT");
        let pt = unsafe { &mut *(pt_phys as *mut [u64; 512]) };
        unsafe { write_bytes(pt.as_mut_ptr(), 0, 4096) };
        kernel_pd[pd_idx] = pt_phys | 0x3;

        // Map 4KB pages in this PT
        let mut mapped_count = 0;
        for pt_idx in 0..512 {
            let virt_addr = virt_2mb.wrapping_add(pt_idx as u64 * 0x1000);

            // Only map pages that are within the kernel virtual range
            if virt_addr >= kernel_virt_base && virt_addr < kernel_virt_end {
                // Calculate corresponding physical address using aligned base addresses
                let phys_addr = kernel_phys_start_aligned
                    .wrapping_add(virt_addr.wrapping_sub(kernel_virt_base));
                pt[pt_idx] = phys_addr | PTE_PRESENT_WRITABLE;
                mapped_count += 1;
            }
        }

        if block_idx == 0 {
            // Save first PT for verification
            first_pt_slice = Some(pt);
        }
    }

    klibcluu::log_dec(
        klibcluu::LogLevel::Trace,
        "  Kernel mapped with 4KB pages (",
        num_2mb_blocks as u64,
    );
    klibcluu::info(" blocks)");
    klibcluu::log_hex(
        klibcluu::LogLevel::Trace,
        "  PML4 ready at: 0x",
        pml4_phys.as_u64(),
    );

    // CRITICAL VERIFICATION: Check that essential mappings are in place
    // before switching CR3, otherwise we'll triple fault!
    klibcluu::info("Verifying critical mappings...");

    // Test 1: Verify current stack is mapped
    let rsp: u64;
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nostack, nomem));
    }
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "  Current RSP: 0x", rsp);

    // Calculate which page the stack is on
    let stack_page = rsp & !0xFFF;
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "  Stack page: 0x", stack_page);

    // Check if stack_page was actually in the mapped range during loop
    if stack_page < kernel_virt_base {
        klibcluu::error("  ERROR: Stack page is BEFORE kernel_virt_base!");
        panic!("Stack page not mapped - too low");
    }
    if stack_page >= kernel_virt_end {
        klibcluu::error("  ERROR: Stack page is AFTER kernel_virt_end!");
        klibcluu::log_hex(
            klibcluu::LogLevel::Error,
            "    kernel_virt_end: 0x",
            kernel_virt_end,
        );
        panic!("Stack page not mapped - too high");
    }

    // Test 2: Read back the page table entry for the stack page to verify it was written
    klibcluu::info("  Verifying stack page table entry...");
    let stack_pml4_idx = ((stack_page >> 39) & 0x1FF) as usize;
    let stack_pdpt_idx = ((stack_page >> 30) & 0x1FF) as usize;
    let stack_pd_idx = ((stack_page >> 21) & 0x1FF) as usize;
    let stack_pt_idx = ((stack_page >> 12) & 0x1FF) as usize;

    klibcluu::log_dec(
        klibcluu::LogLevel::Debug,
        "    Stack indices: PML4[",
        stack_pml4_idx as u64,
    );
    klibcluu::log_dec(klibcluu::LogLevel::Debug, "] PDPT[", stack_pdpt_idx as u64);
    klibcluu::log_dec(klibcluu::LogLevel::Debug, "] PD[", stack_pd_idx as u64);
    klibcluu::log_dec(klibcluu::LogLevel::Debug, "] PT[", stack_pt_idx as u64);
    klibcluu::info("]");

    // Read the PT entry for the stack
    // We can use the virtual address slices we already have!
    // Stack should be at: PML4[511] -> PDPT[511] -> PD[511] -> PT[60]
    let pml4_entry = pml4[stack_pml4_idx];
    klibcluu::log_hex(
        klibcluu::LogLevel::Debug,
        "    PML4[511] entry: 0x",
        pml4_entry,
    );

    if pml4_entry & 0x1 == 0 {
        klibcluu::error("  ERROR: PML4[511] entry NOT PRESENT!");
        panic!("Kernel PML4 entry not present");
    }

    let pdpt_entry = kernel_pdpt[stack_pdpt_idx];
    klibcluu::log_hex(
        klibcluu::LogLevel::Debug,
        "    PDPT[511] entry: 0x",
        pdpt_entry,
    );

    if pdpt_entry & 0x1 == 0 {
        klibcluu::error("  ERROR: PDPT[511] entry NOT PRESENT!");
        panic!("Kernel PDPT entry not present");
    }

    let pd_entry = kernel_pd[stack_pd_idx];
    klibcluu::log_hex(klibcluu::LogLevel::Debug, "    PD[511] entry: 0x", pd_entry);

    if pd_entry & 0x1 == 0 {
        klibcluu::error("  ERROR: PD[511] entry NOT PRESENT!");
        panic!("Kernel PD entry not present");
    }

    // Verify the actual stack PTE
    let pt_phys = pd_entry & !0xFFF;
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "    PT phys: 0x", pt_phys);

    if let Some(first_pt) = first_pt_slice {
        let stack_pte = first_pt[stack_pt_idx];
        klibcluu::log_hex(
            klibcluu::LogLevel::Trace,
            "    Stack PT[60] entry: 0x",
            stack_pte,
        );

        if stack_pte & 0x1 == 0 {
            klibcluu::error("  ERROR: Stack PTE NOT PRESENT!");
            panic!("Stack page table entry not present");
        }

        let stack_phys_from_pte = stack_pte & !0xFFF;
        klibcluu::log_hex(
            klibcluu::LogLevel::Trace,
            "    Stack maps to phys: 0x",
            stack_phys_from_pte,
        );
    }

    klibcluu::info("  Page table hierarchy verified");

    // Check if stack is in kernel range
    if stack_page >= kernel_virt_base && stack_page < kernel_virt_end {
        klibcluu::info("  Stack is in kernel range - OK");

        // Calculate what physical address the stack should map to
        let stack_offset = stack_page.wrapping_sub(kernel_virt_base);
        let expected_stack_phys = kernel_phys_start.wrapping_add(stack_offset);
        klibcluu::log_hex(
            klibcluu::LogLevel::Debug,
            "    Expected stack phys: 0x",
            expected_stack_phys,
        );
    } else {
        klibcluu::error("  ERROR: Stack is NOT in kernel range!");
        klibcluu::log_hex(klibcluu::LogLevel::Error, "    Stack page: 0x", stack_page);
        klibcluu::log_hex(
            klibcluu::LogLevel::Error,
            "    Kernel start: 0x",
            kernel_virt_base,
        );
        klibcluu::log_hex(
            klibcluu::LogLevel::Error,
            "    Kernel end: 0x",
            kernel_virt_end,
        );
        panic!("Stack not in mapped kernel range!");
    }

    // Test 3: Verify current instruction pointer is mapped
    klibcluu::info("  Verifying RIP page table entry...");
    let rip: u64;
    unsafe {
        core::arch::asm!("lea {}, [rip]", out(reg) rip, options(nostack, nomem));
    }
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "  Current RIP: 0x", rip);

    let rip_page = rip & !0xFFF;
    if rip_page >= kernel_virt_base && rip_page < kernel_virt_end {
        klibcluu::info("  RIP is in kernel range - OK");

        // Verify RIP PTE
        if let Some(first_pt) = first_pt_slice {
            let rip_pt_idx = ((rip_page >> 12) & 0x1FF) as usize;
            let rip_pte = first_pt[rip_pt_idx]; // Same PT as stack (both in kernel)
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "    RIP PT entry: 0x", rip_pte);

            if rip_pte & 0x1 == 0 {
                klibcluu::error("  ERROR: RIP page table entry NOT PRESENT!");
                panic!("RIP PTE not present");
            }
        }
    } else {
        klibcluu::error("  ERROR: RIP is NOT in kernel range!");
        panic!("Instruction pointer not in mapped kernel range!");
    }

    klibcluu::info("  All critical mappings verified");

    pml4_phys
}

/// Switch to new page tables by loading CR3
///
/// This is the critical moment where we abandon BOOTBOOT's page tables
/// and switch to our own with physmap.
///
/// # Safety
///
/// - New page tables must be properly set up
/// - Kernel must be mapped in new page tables
/// - Must be called with interrupts disabled
pub unsafe fn switch_to_page_tables(pml4_phys: PhysAddr) {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::PhysFrame;

    klibcluu::info("Switching CR3 to new page tables...");

    // Ensure all page table writes are visible before CR3 switch
    core::arch::asm!("mfence", options(nostack, nomem));

    let frame = PhysFrame::containing_address(pml4_phys);

    unsafe {
        Cr3::write(frame, x86_64::registers::control::Cr3Flags::empty());
    }

    // DO NOT log here - logging might access unmapped memory!
    // Success is indicated by not crashing
}

/// Map kernel heap region with 4KB pages
///
/// Simple, straightforward approach: allocate one frame per page and map it.
/// Follows the same pattern as the old implementation for reliability.
///
/// # Arguments
///
/// * `virt_start` - Starting virtual address
/// * `size` - Size in bytes
///
/// # Safety
///
/// - Must be called after PMM and physmap are initialized
/// - Virtual range must not overlap existing mappings
/// - Must use current CR3 (kernel page tables)
pub unsafe fn map_heap_region(virt_start: u64, size: u64) -> Result<(), &'static str> {
    use core::ptr::write_bytes;
    use x86_64::registers::control::Cr3;

    let page_count = (size + 0xFFF) / 0x1000;

    klibcluu::log_dec(klibcluu::LogLevel::Trace, "  Mapping heap: ", page_count);
    klibcluu::info(" pages");

    for i in 0..page_count {
        let virt_addr = virt_start + (i * 0x1000);

        // Allocate one physical frame
        let phys_frame = crate::mm::pmm_simple::alloc_frame().ok_or("Out of memory for heap")?;

        // Map the page
        unsafe {
            map_single_4k_page(virt_addr, phys_frame, true, false)?;
        }
    }

    klibcluu::info("  Heap mapped successfully");
    Ok(())
}

/// Map a single 4KB page (helper for heap mapping)
///
/// # Arguments
///
/// * `virt` - Virtual address
/// * `phys` - Physical address
/// * `writable` - Whether page should be writable
/// * `user` - Whether page should be user-accessible
unsafe fn map_single_4k_page(
    virt: u64,
    phys: u64,
    writable: bool,
    user: bool,
) -> Result<(), &'static str> {
    use core::ptr::write_bytes;
    use x86_64::registers::control::Cr3;

    let (pml4_frame, _) = Cr3::read();
    let pml4_phys = pml4_frame.start_address().as_u64();

    // Calculate page table indices
    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx = ((virt >> 12) & 0x1FF) as usize;

    // Flags for intermediate tables (always present + writable)
    let table_flags = PTE_PRESENT_WRITABLE;

    // Flags for final PTE
    let mut page_flags = pte_flags::PRESENT;
    if writable {
        page_flags |= pte_flags::WRITABLE;
    }
    if user {
        page_flags |= pte_flags::USER;
    }

    // Access PML4
    let pml4 = unsafe { &mut *(super::physmap::phys_to_virt_u64(pml4_phys) as *mut [u64; 512]) };

    // Get or create PDPT
    let pdpt_phys = if pml4[pml4_idx] & 0x1 != 0 {
        pml4[pml4_idx] & !0xFFF
    } else {
        let pdpt_phys =
            crate::mm::pmm_simple::alloc_frame().ok_or("Out of memory allocating PDPT")?;
        let pdpt_virt = unsafe { super::physmap::phys_to_virt_u64(pdpt_phys) };
        unsafe { write_bytes(pdpt_virt as *mut u8, 0, 4096) };
        pml4[pml4_idx] = pdpt_phys | table_flags;
        pdpt_phys
    };

    let pdpt = unsafe { &mut *(super::physmap::phys_to_virt_u64(pdpt_phys) as *mut [u64; 512]) };

    // Get or create PD
    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        pdpt[pdpt_idx] & !0xFFF
    } else {
        let pd_phys = crate::mm::pmm_simple::alloc_frame().ok_or("Out of memory allocating PD")?;
        let pd_virt = unsafe { super::physmap::phys_to_virt_u64(pd_phys) };
        unsafe { write_bytes(pd_virt as *mut u8, 0, 4096) };
        pdpt[pdpt_idx] = pd_phys | table_flags;
        pd_phys
    };

    let pd = unsafe { &mut *(super::physmap::phys_to_virt_u64(pd_phys) as *mut [u64; 512]) };

    // Get or create PT
    let pt_phys = if pd[pd_idx] & 0x1 != 0 {
        // Check if it's a huge page
        if pd[pd_idx] & pte_flags::HUGE != 0 {
            return Err("Trying to map 4KB page where 2MB huge page exists");
        }
        pd[pd_idx] & !0xFFF
    } else {
        let pt_phys = crate::mm::pmm_simple::alloc_frame().ok_or("Out of memory allocating PT")?;
        let pt_virt = unsafe { super::physmap::phys_to_virt_u64(pt_phys) };
        unsafe { write_bytes(pt_virt as *mut u8, 0, 4096) };
        pd[pd_idx] = pt_phys | table_flags;
        pt_phys
    };

    let pt = unsafe { &mut *(super::physmap::phys_to_virt_u64(pt_phys) as *mut [u64; 512]) };

    // Map the page
    if pt[pt_idx] & 0x1 != 0 {
        return Err("Page already mapped");
    }
    pt[pt_idx] = phys | page_flags;

    // Flush TLB for this page
    unsafe {
        core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
    }

    Ok(())
}

/// Map a region using 4KB pages
///
/// Used for partial regions, MMIO, and other areas requiring fine granularity.
unsafe fn map_4kb_region(
    pdpt: &mut [u64],
    start_phys: u64,
    end_phys: u64,
    page_count: &mut u64,
    kernel_virt_base: u64,
    kernel_phys_start: u64,
) {
    use core::ptr::write_bytes;

    // Calculate which GB this region falls into
    let start_gb = (start_phys / 0x4000_0000) as usize;
    let end_gb = ((end_phys - 1) / 0x4000_0000) as usize;

    for gb_idx in start_gb..=end_gb.min(511) {
        // Allocate or reuse PD for this GB
        let pd_frame = if pdpt[gb_idx] & 0x1 != 0 {
            // Already allocated (from 2MB mapping)
            // Check if it's a huge page or PD
            if pdpt[gb_idx] & 0x80 != 0 {
                // It's a 1GB huge page, we need to split it
                // For now, panic - this shouldn't happen with our allocation strategy
                panic!("Cannot split 1GB huge page");
            }
            (pdpt[gb_idx] & !0xFFF) as *mut u8
        } else {
            // Allocate new PD from PMM
            let pd_phys =
                crate::mm::pmm_simple::alloc_frame().expect("Failed to allocate PD for 4KB region");
            unsafe { write_bytes(pd_phys as *mut u8, 0, 4096) };
            pdpt[gb_idx] = pd_phys | 0x3;
            pd_phys as *mut u8
        };

        let pd = unsafe { core::slice::from_raw_parts_mut(pd_frame as *mut u64, 512) };
        let gb_base = (gb_idx as u64) * 0x4000_0000;

        // Find which 2MB blocks in this GB need 4KB pages
        let start_2mb = if start_phys > gb_base {
            ((start_phys - gb_base) / 0x20_0000) as usize
        } else {
            0
        };
        let end_2mb = (((end_phys - gb_base).min(0x4000_0000) + 0x1F_FFFF) / 0x20_0000) as usize;

        for block_2mb in start_2mb..end_2mb.min(512) {
            // Allocate PT for this 2MB block from PMM
            let pt_phys =
                crate::mm::pmm_simple::alloc_frame().expect("Failed to allocate PT for 4KB pages");
            unsafe { write_bytes(pt_phys as *mut u8, 0, 4096) };
            pd[block_2mb] = pt_phys | 0x3; // Present | Writable (NOT huge page)

            let pt = unsafe { core::slice::from_raw_parts_mut(pt_phys as *mut u64, 512) };
            let block_base = gb_base + (block_2mb as u64) * 0x20_0000;

            // Map 4KB pages in this PT
            for pt_idx in 0..512 {
                let phys_addr = block_base + (pt_idx as u64) * 0x1000;
                if phys_addr >= start_phys && phys_addr < end_phys {
                    pt[pt_idx] = phys_addr | 0x3; // Present | Writable (4KB page)
                    *page_count += 1;
                }
            }
        }
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

    // ═══════════════════════════════════════════════════════════════════════
    // Page Table Creation Tests
    // ═══════════════════════════════════════════════════════════════════════

    /// Test page table creation with aligned memory (no partial region)
    #[test]
    fn test_page_table_creation_aligned() {
        // Test with 4GB of RAM (2MB aligned)
        let max_phys = 0x1_0000_0000u64; // 4 GB
        let kernel_start = 0x100000u64;
        let kernel_end = 0x200000u64;

        unsafe {
            let pml4_phys = create_initial_page_tables(max_phys, kernel_start, kernel_end);

            // Verify PML4 address is valid (non-zero, page-aligned)
            assert_ne!(pml4_phys.as_u64(), 0);
            assert_eq!(pml4_phys.as_u64() % 4096, 0);

            // Verify PML4 is within reasonable bounds (bump allocator heap)
            let pml4_addr = pml4_phys.as_u64();
            assert!(pml4_addr > 0, "PML4 address should be non-zero");
        }
    }

    /// Test page table creation with unaligned memory (has partial region)
    #[test]
    fn test_page_table_creation_unaligned() {
        // Test with 4GB + 1MB (not 2MB aligned)
        let max_phys = 0x1_0010_0000u64; // 4 GB + 1 MB
        let kernel_start = 0x100000u64;
        let kernel_end = 0x200000u64;

        unsafe {
            let pml4_phys = create_initial_page_tables(max_phys, kernel_start, kernel_end);

            // Should succeed and create both 2MB and 4KB pages
            assert_ne!(pml4_phys.as_u64(), 0);
            assert_eq!(pml4_phys.as_u64() % 4096, 0);
        }
    }

    /// Test page table creation with small memory (< 2MB)
    #[test]
    fn test_page_table_creation_small_memory() {
        // Test with 1MB of RAM (all 4KB pages)
        let max_phys = 0x10_0000u64; // 1 MB
        let kernel_start = 0x100000u64;
        let kernel_end = 0x180000u64;

        unsafe {
            let pml4_phys = create_initial_page_tables(max_phys, kernel_start, kernel_end);

            // Should succeed using only 4KB pages
            assert_ne!(pml4_phys.as_u64(), 0);
            assert_eq!(pml4_phys.as_u64() % 4096, 0);
        }
    }

    /// Test page table creation with large memory (16GB)
    #[test]
    fn test_page_table_creation_large_memory() {
        // Test with 16GB of RAM
        let max_phys = 0x4_0000_0000u64; // 16 GB
        let kernel_start = 0x100000u64;
        let kernel_end = 0x200000u64;

        unsafe {
            let pml4_phys = create_initial_page_tables(max_phys, kernel_start, kernel_end);

            // Should succeed with mostly 2MB pages
            assert_ne!(pml4_phys.as_u64(), 0);
            assert_eq!(pml4_phys.as_u64() % 4096, 0);
        }
    }

    /// Test that PML4 entries are properly set
    #[test]
    fn test_pml4_structure() {
        let max_phys = 0x8000_0000u64; // 2 GB
        let kernel_start = 0x100000u64;
        let kernel_end = 0x200000u64;

        unsafe {
            let pml4_phys = create_initial_page_tables(max_phys, kernel_start, kernel_end);

            // Access PML4 via identity mapping (during tests, we're still in BOOTBOOT)
            let pml4_ptr = pml4_phys.as_u64() as *const u64;
            let pml4 = core::slice::from_raw_parts(pml4_ptr, 512);

            // Check physmap entry (index 256 for 0xffff_8000_0000_0000)
            let physmap_idx = ((crate::mm::physmap::PHYS_MAP_BASE >> 39) & 0x1FF) as usize;
            let physmap_entry = pml4[physmap_idx];

            // Should be present
            assert_ne!(
                physmap_entry & 0x1,
                0,
                "Physmap PML4 entry should be present"
            );
            // Should be writable
            assert_ne!(
                physmap_entry & 0x2,
                0,
                "Physmap PML4 entry should be writable"
            );

            // Check kernel entry (index 511 for high addresses)
            let kernel_entry = pml4[511];
            assert_ne!(kernel_entry & 0x1, 0, "Kernel PML4 entry should be present");
            assert_ne!(
                kernel_entry & 0x2,
                0,
                "Kernel PML4 entry should be writable"
            );

            // All other entries should be zero (not present)
            for (i, &entry) in pml4.iter().enumerate() {
                if i != physmap_idx && i != 511 {
                    assert_eq!(entry & 0x1, 0, "PML4 entry {} should not be present", i);
                }
            }
        }
    }

    /// Test aligned region calculation
    #[test]
    fn test_aligned_region_calculation() {
        // 4GB exactly (aligned)
        let max_phys = 0x1_0000_0000u64;
        let aligned_end = max_phys & !(0x1F_FFFF);
        assert_eq!(aligned_end, max_phys);

        // 4GB + 1MB (unaligned)
        let max_phys = 0x1_0010_0000u64;
        let aligned_end = max_phys & !(0x1F_FFFF);
        assert_eq!(aligned_end, 0x1_0000_0000u64);

        // 2MB exactly (aligned)
        let max_phys = 0x20_0000u64;
        let aligned_end = max_phys & !(0x1F_FFFF);
        assert_eq!(aligned_end, max_phys);

        // 1MB (unaligned)
        let max_phys = 0x10_0000u64;
        let aligned_end = max_phys & !(0x1F_FFFF);
        assert_eq!(aligned_end, 0);
    }

    /// Test page count calculations
    #[test]
    fn test_page_count_calculations() {
        // 4GB aligned: should be 2048 2MB pages, 0 4KB pages
        let max_phys = 0x1_0000_0000u64;
        let aligned_end = max_phys & !(0x1F_FFFF);
        let pages_2mb = aligned_end / 0x20_0000;
        let partial_size = max_phys - aligned_end;
        let pages_4kb = if partial_size > 0 {
            (partial_size + 0xFFF) / 0x1000
        } else {
            0
        };

        assert_eq!(pages_2mb, 2048);
        assert_eq!(pages_4kb, 0);

        // 4GB + 1MB: should be 2048 2MB pages, 256 4KB pages
        let max_phys = 0x1_0010_0000u64;
        let aligned_end = max_phys & !(0x1F_FFFF);
        let pages_2mb = aligned_end / 0x20_0000;
        let partial_size = max_phys - aligned_end;
        let pages_4kb = if partial_size > 0 {
            (partial_size + 0xFFF) / 0x1000
        } else {
            0
        };

        assert_eq!(pages_2mb, 2048);
        assert_eq!(pages_4kb, 256);
    }

    /// Test PML4 index calculation for physmap
    #[test]
    fn test_physmap_pml4_index() {
        let physmap_base = crate::mm::physmap::PHYS_MAP_BASE;
        let pml4_idx = ((physmap_base >> 39) & 0x1FF) as usize;

        // 0xffff_8000_0000_0000 should map to PML4 index 256
        assert_eq!(pml4_idx, 256);
    }

    /// Test kernel PML4 index (should be 511 for high addresses)
    #[test]
    fn test_kernel_pml4_index() {
        let kernel_virt = 0xffffffffffe02000u64;
        let pml4_idx = ((kernel_virt >> 39) & 0x1FF) as usize;

        assert_eq!(pml4_idx, 511);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Existing Tests
    // ═══════════════════════════════════════════════════════════════════════

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
