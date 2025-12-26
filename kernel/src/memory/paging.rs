/*
 * Paging and Virtual Memory Management (New Implementation)
 *
 * This module provides page table manipulation using physmap for all access.
 * No CR3 switching hacks needed - we can manipulate any page table directly.
 *
 * KEY IMPROVEMENTS:
 * - All page table access via physmap (no CR3 switching)
 * - Works on any root PhysAddr (not just current CR3)
 * - Clean separation of page table walking and mapping
 * - Support for building new address spaces from scratch
 *
 * ARCHITECTURE:
 * - x86_64 4-level paging: PML4 → PDPT → PD → PT → 4K page
 * - Each level is 512 entries (9 bits)
 * - Entry format: [physical address (12-51)] | [flags (0-11, 52-63)]
 */

use crate::memory::{
    phys as pmm, physmap,
    types::{PageTableFlags, PhysAddr, PhysFrame, VirtAddr},
};

/// Get a pointer to physical memory
///
/// During bootstrap (before physmap is mapped), uses BOOTBOOT's identity mapping.
/// After switching to our own page tables, uses physmap.
///
/// # Safety
/// - During bootstrap: BOOTBOOT must have identity mapped the physical address
/// - After bootstrap: Physmap must be properly set up
#[inline]
unsafe fn phys_ptr<T>(phys: PhysAddr) -> *mut T {
    if physmap::is_active() {
        unsafe { physmap::phys_ptr(phys) }  // Fixed: was calling itself recursively!
    } else {
        // BOOTBOOT identity maps all RAM - access physical address directly
        phys.as_u64() as *mut T
    }
}

/// Page table entry
#[repr(transparent)]
#[derive(Clone, Copy)]
struct PageTableEntry(u64);

impl PageTableEntry {
    /// Create empty entry
    fn new() -> Self {
        Self(0)
    }

    /// Get physical address from entry
    fn addr(&self) -> PhysAddr {
        PhysAddr::new(self.0 & 0x000f_ffff_ffff_f000)
    }

    /// Set physical address and flags
    fn set(&mut self, addr: PhysAddr, flags: PageTableFlags) {
        let addr_u64 = addr.as_u64();

        // CRITICAL VALIDATION: Ensure we're storing a physical address, not virtual
        // This catches the #1 cause of CR3 switch failures
        assert!(addr_u64 & 0xfff == 0,
                "Page table entry address must be 4KB aligned, got 0x{:x}", addr_u64);

        // Ensure address is physical (not a physmap virtual address)
        // Physmap starts at 0xffff_8000_0000_0000, so any address >= that is virtual
        if addr_u64 >= physmap::PHYS_MAP_BASE {
            panic!("Attempting to store virtual address 0x{:x} in page table entry! \
                    Must use physical address.", addr_u64);
        }

        // Note: We allow addresses beyond max_phys for MMIO regions (framebuffer, PCI, etc.)
        // Those are valid physical addresses even if they're not RAM

        let addr_bits = addr_u64 & 0x000f_ffff_ffff_f000;
        let flags_bits = flags.bits();
        self.0 = addr_bits | flags_bits;
    }

    /// Check if entry is present
    fn is_present(&self) -> bool {
        (self.0 & 0x1) != 0
    }

    /// Clear entry
    fn clear(&mut self) {
        self.0 = 0;
    }

    /// Get flags
    fn flags(&self) -> PageTableFlags {
        PageTableFlags::from_bits_truncate(self.0)
    }
}

/// Page table (512 entries)
#[repr(align(4096))]
struct PageTable {
    entries: [PageTableEntry; 512],
}

impl PageTable {
    /// Get entry at index
    fn entry(&self, index: usize) -> PageTableEntry {
        self.entries[index]
    }

    /// Get mutable entry at index
    fn entry_mut(&mut self, index: usize) -> &mut PageTableEntry {
        &mut self.entries[index]
    }

    /// Zero out all entries
    fn zero(&mut self) {
        for entry in &mut self.entries {
            entry.clear();
        }
    }
}

/// Extract page table indices from virtual address
fn page_table_indices(virt: VirtAddr) -> (usize, usize, usize, usize) {
    let addr = virt.as_u64();
    let pml4_idx = ((addr >> 39) & 0x1ff) as usize;
    let pdpt_idx = ((addr >> 30) & 0x1ff) as usize;
    let pd_idx = ((addr >> 21) & 0x1ff) as usize;
    let pt_idx = ((addr >> 12) & 0x1ff) as usize;
    (pml4_idx, pdpt_idx, pd_idx, pt_idx)
}

/// Walk page tables to find mapping for virtual address
///
/// Returns the physical address and flags if mapped, None otherwise.
pub fn translate(root: PhysAddr, virt: VirtAddr) -> Option<(PhysAddr, PageTableFlags)> {
    let (pml4_idx, pdpt_idx, pd_idx, pt_idx) = page_table_indices(virt);

    // Access PML4 via physmap
    let pml4_ptr = unsafe { phys_ptr::<PageTable>(root) };
    let pml4 = unsafe { &*pml4_ptr };
    let pml4e = pml4.entry(pml4_idx);
    if !pml4e.is_present() {
        return None;
    }

    // Access PDPT via physmap
    let pdpt_ptr = unsafe { phys_ptr::<PageTable>(pml4e.addr()) };
    let pdpt = unsafe { &*pdpt_ptr };
    let pdpte = pdpt.entry(pdpt_idx);
    if !pdpte.is_present() {
        return None;
    }

    // Check for 1GB page
    if (pdpte.flags().bits() & (1 << 7)) != 0 {
        // 1GB page
        let offset = virt.as_u64() & 0x3fff_ffff;
        let phys = PhysAddr::new((pdpte.addr().as_u64() & !0x3fff_ffff) + offset);
        return Some((phys, pdpte.flags()));
    }

    // Access PD via physmap
    let pd_ptr = unsafe { phys_ptr::<PageTable>(pdpte.addr()) };
    let pd = unsafe { &*pd_ptr };
    let pde = pd.entry(pd_idx);
    if !pde.is_present() {
        return None;
    }

    // Check for 2MB page
    if (pde.flags().bits() & (1 << 7)) != 0 {
        // 2MB page
        let offset = virt.as_u64() & 0x1f_ffff;
        let phys = PhysAddr::new((pde.addr().as_u64() & !0x1f_ffff) + offset);
        return Some((phys, pde.flags()));
    }

    // Access PT via physmap
    let pt_ptr = unsafe { phys_ptr::<PageTable>(pde.addr()) };
    let pt = unsafe { &*pt_ptr };
    let pte = pt.entry(pt_idx);
    if !pte.is_present() {
        return None;
    }

    // 4KB page
    let offset = virt.as_u64() & 0xfff;
    let phys = PhysAddr::new(pte.addr().as_u64() + offset);
    Some((phys, pte.flags()))
}

/// Map a 4K page in the given page table root
///
/// Allocates intermediate page tables as needed.
///
/// # Arguments
/// * `root` - Physical address of PML4
/// * `virt` - Virtual address to map (will be page-aligned)
/// * `phys` - Physical address to map to (will be page-aligned)
/// * `flags` - Page table flags
///
/// # Returns
/// * `Ok(())` - Mapping succeeded
/// * `Err(&str)` - Mapping failed (out of memory, already mapped)
pub fn map_4k(
    root: PhysAddr,
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let virt_aligned = VirtAddr::new(virt.as_u64() & !0xfff);
    let phys_aligned = PhysAddr::new(phys.as_u64() & !0xfff);

    let (pml4_idx, pdpt_idx, pd_idx, pt_idx) = page_table_indices(virt_aligned);

    // Flags for intermediate tables (present + writable + user if needed)
    let mut table_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    if flags.contains(PageTableFlags::USER_ACCESSIBLE) {
        table_flags |= PageTableFlags::USER_ACCESSIBLE;
    }

    // Access PML4
    let pml4_ptr = unsafe { phys_ptr::<PageTable>(root) };
    let pml4 = unsafe { &mut *pml4_ptr };

    // Ensure PDPT exists
    let pdpt_addr = if !pml4.entry(pml4_idx).is_present() {
        let frame = pmm::alloc_frame().ok_or("Out of memory allocating PDPT")?;
        let pdpt_addr = PhysAddr::new(frame.start_address());

        // Zero out new PDPT
        let pdpt_ptr = unsafe { phys_ptr::<PageTable>(pdpt_addr) };
        unsafe { (*pdpt_ptr).zero() };

        pml4.entry_mut(pml4_idx).set(pdpt_addr, table_flags);
        pdpt_addr
    } else {
        pml4.entry(pml4_idx).addr()
    };

    // Access PDPT
    let pdpt_ptr = unsafe { phys_ptr::<PageTable>(pdpt_addr) };
    let pdpt = unsafe { &mut *pdpt_ptr };

    // Ensure PD exists
    let pd_addr = if !pdpt.entry(pdpt_idx).is_present() {
        let frame = pmm::alloc_frame().ok_or("Out of memory allocating PD")?;
        let pd_addr = PhysAddr::new(frame.start_address());

        // Zero out new PD
        let pd_ptr = unsafe { phys_ptr::<PageTable>(pd_addr) };
        unsafe { (*pd_ptr).zero() };

        pdpt.entry_mut(pdpt_idx).set(pd_addr, table_flags);
        pd_addr
    } else {
        pdpt.entry(pdpt_idx).addr()
    };

    // Access PD via physmap
    let pd_ptr = unsafe { phys_ptr::<PageTable>(pd_addr) };
    let pd = unsafe { &mut *pd_ptr };

    // Ensure PT exists
    let pt_addr = if !pd.entry(pd_idx).is_present() {
        let frame = pmm::alloc_frame().ok_or("Out of memory allocating PT")?;
        let pt_addr = PhysAddr::new(frame.start_address());

        // Zero out new PT
        let pt_ptr = unsafe { phys_ptr::<PageTable>(pt_addr) };
        unsafe { (*pt_ptr).zero() };

        pd.entry_mut(pd_idx).set(pt_addr, table_flags);
        pt_addr
    } else {
        pd.entry(pd_idx).addr()
    };

    // Access PT via physmap
    let pt_ptr = unsafe { phys_ptr::<PageTable>(pt_addr) };
    let pt = unsafe { &mut *pt_ptr };

    // Check if already mapped
    if pt.entry(pt_idx).is_present() {
        return Err("Page already mapped");
    }

    // Map the page
    pt.entry_mut(pt_idx)
        .set(phys_aligned, flags | PageTableFlags::PRESENT);

    Ok(())
}

/// Unmap a 4K page
///
/// # Arguments
/// * `root` - Physical address of PML4
/// * `virt` - Virtual address to unmap
///
/// # Returns
/// * `Ok(PhysAddr)` - Physical address that was mapped (caller should free the frame)
/// * `Err(&str)` - Unmapping failed (not mapped)
pub fn unmap_4k(root: PhysAddr, virt: VirtAddr) -> Result<PhysAddr, &'static str> {
    let virt_aligned = VirtAddr::new(virt.as_u64() & !0xfff);
    let (pml4_idx, pdpt_idx, pd_idx, pt_idx) = page_table_indices(virt_aligned);

    // Walk to PT
    let pml4_ptr = unsafe { phys_ptr::<PageTable>(root) };
    let pml4 = unsafe { &mut *pml4_ptr };
    if !pml4.entry(pml4_idx).is_present() {
        return Err("Page not mapped (PML4)");
    }

    let pdpt_ptr = unsafe { phys_ptr::<PageTable>(pml4.entry(pml4_idx).addr()) };
    let pdpt = unsafe { &mut *pdpt_ptr };
    if !pdpt.entry(pdpt_idx).is_present() {
        return Err("Page not mapped (PDPT)");
    }

    let pd_ptr = unsafe { phys_ptr::<PageTable>(pdpt.entry(pdpt_idx).addr()) };
    let pd = unsafe { &mut *pd_ptr };
    if !pd.entry(pd_idx).is_present() {
        return Err("Page not mapped (PD)");
    }

    let pt_ptr = unsafe { phys_ptr::<PageTable>(pd.entry(pd_idx).addr()) };
    let pt = unsafe { &mut *pt_ptr };
    if !pt.entry(pt_idx).is_present() {
        return Err("Page not mapped (PT)");
    }

    // Get physical address before clearing
    let phys = pt.entry(pt_idx).addr();

    // Clear entry
    pt.entry_mut(pt_idx).clear();

    // TODO: Free empty page tables (optimization)

    Ok(phys)
}

/// Map a range of virtual addresses to specific physical addresses
///
/// Maps virt_start..virt_start+size to phys_start..phys_start+size.
///
/// # Arguments
/// * `root` - Physical address of PML4
/// * `virt_start` - Starting virtual address
/// * `phys_start` - Starting physical address
/// * `size` - Size in bytes (will be rounded up to page boundary)
/// * `flags` - Page table flags
///
/// # Returns
/// * `Ok(())` - All pages mapped successfully
/// * `Err(&str)` - Mapping failed partway through
pub fn map_range_4k_phys(
    root: PhysAddr,
    virt_start: VirtAddr,
    phys_start: PhysAddr,
    size: u64,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let page_count = (size + 0xfff) / 0x1000;

    // Log progress for large mappings (> 10MB)
    let log_progress = size > 10 * 1024 * 1024;
    let progress_interval = 16384; // Log every 16K pages (64 MB)

    for i in 0..page_count {
        let virt = VirtAddr::new(virt_start.as_u64() + i * 0x1000);
        let phys = PhysAddr::new(phys_start.as_u64() + i * 0x1000);

        map_4k(root, virt, phys, flags)?;

        if log_progress && i > 0 && i % progress_interval == 0 {
            log::debug!(
                "  Mapped {} / {} pages ({} MB / {} MB)",
                i,
                page_count,
                (i * 4096) / (1024 * 1024),
                (page_count * 4096) / (1024 * 1024)
            );
        }
    }

    Ok(())
}

/// Map a range of virtual addresses to newly allocated physical frames
///
/// # Arguments
/// * `root` - Physical address of PML4
/// * `virt_start` - Starting virtual address
/// * `size` - Size in bytes (will be rounded up to page boundary)
/// * `flags` - Page table flags
///
/// # Returns
/// * `Ok(())` - All pages mapped successfully
/// * `Err(&str)` - Mapping failed partway through (some pages may be mapped)
pub fn map_range_4k(
    root: PhysAddr,
    virt_start: VirtAddr,
    size: u64,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let page_count = (size + 0xfff) / 0x1000;

    for i in 0..page_count {
        let virt = VirtAddr::new(virt_start.as_u64() + i * 0x1000);
        let frame = pmm::alloc_frame().ok_or("Out of physical memory")?;
        let phys = PhysAddr::new(frame.start_address());

        map_4k(root, virt, phys, flags)?;
    }

    Ok(())
}

/// Unmap a range of virtual addresses and free backing frames
///
/// # Arguments
/// * `root` - Physical address of PML4
/// * `virt_start` - Starting virtual address
/// * `size` - Size in bytes (will be rounded up to page boundary)
///
/// # Returns
/// * `Ok(())` - All pages unmapped successfully
/// * `Err(&str)` - Unmapping failed for at least one page
pub fn unmap_range_4k(root: PhysAddr, virt_start: VirtAddr, size: u64) -> Result<(), &'static str> {
    let page_count = (size + 0xfff) / 0x1000;
    let mut any_failed = false;

    for i in 0..page_count {
        let virt = VirtAddr::new(virt_start.as_u64() + i * 0x1000);

        match unmap_4k(root, virt) {
            Ok(phys) => {
                // Free the frame
                pmm::free_frame(PhysFrame::containing_address(phys.as_u64()));
            }
            Err(_) => {
                any_failed = true;
            }
        }
    }

    if any_failed {
        Err("Failed to unmap one or more pages")
    } else {
        Ok(())
    }
}

/// Allocate a new PML4 (page table root)
///
/// Returns a zeroed PML4 ready for use.
pub fn alloc_pml4() -> Result<PhysAddr, &'static str> {
    let frame = pmm::alloc_frame().ok_or("Out of memory allocating PML4")?;
    let pml4_addr = PhysAddr::new(frame.start_address());

    // Zero out PML4
    // During bootstrap, use BOOTBOOT's identity mapping (phys addr = virt addr)
    // After our page tables are active, physmap will work
    let pml4_ptr = if physmap::is_active() {
        unsafe { phys_ptr::<PageTable>(pml4_addr) }
    } else {
        // BOOTBOOT identity maps all RAM - access physical address directly
        pml4_addr.as_u64() as *mut PageTable
    };
    unsafe { (*pml4_ptr).zero() };

    Ok(pml4_addr)
}

/// Copy a PML4 entry from one root to another
///
/// Used for copying kernel half of address space to new user process.
///
/// # Arguments
/// * `src_root` - Source PML4 physical address
/// * `dst_root` - Destination PML4 physical address
/// * `index` - PML4 entry index (0-511)
pub fn copy_pml4_entry(src_root: PhysAddr, dst_root: PhysAddr, index: usize) {
    let src_ptr = unsafe { phys_ptr::<PageTable>(src_root) };
    let dst_ptr = unsafe { phys_ptr::<PageTable>(dst_root) };

    let src = unsafe { &*src_ptr };
    let dst = unsafe { &mut *dst_ptr };

    *dst.entry_mut(index) = src.entry(index);
}

/// Copy kernel half of PML4 (entries 256-511) from source to destination
///
/// This is used when creating a new user address space.
pub fn copy_kernel_half(src_root: PhysAddr, dst_root: PhysAddr) {
    for i in 256..512 {
        copy_pml4_entry(src_root, dst_root, i);
    }
}

/// Map a page with specific permissions (helper)
pub fn map_page_kernel(
    root: PhysAddr,
    virt: VirtAddr,
    phys: PhysAddr,
    writable: bool,
    executable: bool,
) -> Result<(), &'static str> {
    let mut flags = PageTableFlags::PRESENT;
    if writable {
        flags |= PageTableFlags::WRITABLE;
    }
    if !executable {
        flags |= PageTableFlags::NO_EXECUTE;
    }

    map_4k(root, virt, phys, flags)
}

/// Map a page with user-accessible permissions
pub fn map_page_user(
    root: PhysAddr,
    virt: VirtAddr,
    phys: PhysAddr,
    writable: bool,
    executable: bool,
) -> Result<(), &'static str> {
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if writable {
        flags |= PageTableFlags::WRITABLE;
    }
    if !executable {
        flags |= PageTableFlags::NO_EXECUTE;
    }

    map_4k(root, virt, phys, flags)
}

/// Flush TLB for a specific virtual address
///
/// Must be called after modifying currently active page tables.
#[inline]
pub fn flush_tlb(virt: VirtAddr) {
    use x86_64::instructions::tlb;
    tlb::flush(virt);
}

/// Flush entire TLB
///
/// Should be called after CR3 change (though CR3 write already flushes TLB).
#[inline]
pub fn flush_tlb_all() {
    use x86_64::instructions::tlb;
    tlb::flush_all();
}

/// Switch to a different page table root
///
/// Updates CR3 register, which automatically flushes TLB.
pub fn switch_cr3(new_root: PhysAddr) {
    let cr3_value = new_root.as_u64();

    // CRITICAL VALIDATION: Ensure CR3 value is correct format
    // Must be physical address, 4KB aligned, not truncated
    assert!(cr3_value & 0xfff == 0, "CR3 must be 4KB aligned, got 0x{:x}", cr3_value);
    assert!(cr3_value < physmap::max_phys(), "CR3 0x{:x} beyond max physical 0x{:x}",
            cr3_value, physmap::max_phys());
    assert!(cr3_value != 0, "CR3 cannot be NULL");

    // Ensure this is a physical address, not a physmap virtual address
    if cr3_value >= physmap::PHYS_MAP_BASE {
        panic!("CR3 appears to be a virtual address (0x{:x}) instead of physical!", cr3_value);
    }

    log::info!("About to write CR3: 0x{:x} (validated: aligned, physical, in range)", cr3_value);

    unsafe {
        // CRITICAL: Disable interrupts before CR3 switch
        // If an interrupt fires during/after CR3 switch and IDT/IST stacks aren't mapped,
        // we'll triple fault
        let mut rflags: u64;
        core::arch::asm!("pushfq; pop {}", out(reg) rflags, options(nomem));
        let interrupts_enabled = (rflags & 0x200) != 0;

        if interrupts_enabled {
            log::warn!("Interrupts are ENABLED before CR3 switch - this is dangerous!");
            log::info!("Disabling interrupts for CR3 switch...");
            core::arch::asm!("cli", options(nostack, nomem));
        } else {
            log::info!("Interrupts already disabled - safe for CR3 switch");
        }

        // Ensure all page table writes are visible before CR3 switch
        // Use memory fence instead of cache flush (wbinvd was too aggressive)
        log::info!("Ensuring memory ordering with mfence...");

        core::arch::asm!(
            "mfence",
            options(nostack, nomem)
        );

        log::info!("Memory fence complete, writing CR3...");

        // Write CR3 - use explicit u64 type to avoid truncation
        let cr3_u64: u64 = cr3_value;

        // DEBUG: Do minimal operations after CR3 write to isolate the issue
        // This helps determine if the problem is the write itself or something after
        core::arch::asm!(
            "mov cr3, {0}",     // Write CR3
            "nop",              // Simple instruction that doesn't touch memory
            "nop",              // Another nop
            "mov rax, cr3",     // Read CR3 back (register-only operation)
            in(reg) cr3_u64,
            out("rax") _,
            options(nostack, preserves_flags)
        );

        // If we get here, CR3 write succeeded!
        // DO NOT log here - logging might access unmapped memory after CR3 switch!

        // Re-enable interrupts if they were enabled before
        if interrupts_enabled {
            core::arch::asm!("sti", options(nostack, nomem));
        }
    }

    // CR3 switch completed - return without logging to avoid accessing unmapped memory
}

/// Get current CR3 value
pub fn get_current_cr3() -> PhysAddr {
    use x86_64::registers::control::Cr3;

    let (frame, _flags) = Cr3::read();
    frame.start_address()
}

/// Translate virtual address using identity-mapped page tables
///
/// This is a special version of translate() that works BEFORE physmap is initialized.
/// It uses BOOTBOOT's identity mapping to access page tables directly.
///
/// BOOTBOOT identity maps all RAM in both lower half and higher half, so we can
/// access page table physical addresses directly as if they were virtual addresses.
///
/// # Safety
/// Only safe to use while BOOTBOOT's page tables are active (before we switch CR3).
/// After switching to our own page tables, use the normal translate() function.
pub unsafe fn translate_via_identity(root: PhysAddr, virt: VirtAddr) -> Option<PhysAddr> {
    unsafe {
        // Extract page table indices from virtual address
        // 47:39 = PML4 index (9 bits)
        // 38:30 = PDPT index (9 bits)
        // 29:21 = PD index (9 bits)
        // 20:12 = PT index (9 bits)
        // 11:0  = offset (12 bits)
        let virt_u64 = virt.as_u64();
        let pml4_idx = ((virt_u64 >> 39) & 0x1ff) as usize;
        let pdpt_idx = ((virt_u64 >> 30) & 0x1ff) as usize;
        let pd_idx = ((virt_u64 >> 21) & 0x1ff) as usize;
        let pt_idx = ((virt_u64 >> 12) & 0x1ff) as usize;
        let offset = virt_u64 & 0xfff;

        // Access PML4 via identity mapping
        let pml4_ptr = root.as_u64() as *const u64;
        let pml4_entry = core::ptr::read_volatile(pml4_ptr.add(pml4_idx));

        if (pml4_entry & 0x1) == 0 {
            return None; // Not present
        }

        // Access PDPT
        let pdpt_addr = pml4_entry & 0x000f_ffff_ffff_f000;
        let pdpt_ptr = pdpt_addr as *const u64;
        let pdpt_entry = core::ptr::read_volatile(pdpt_ptr.add(pdpt_idx));

        if (pdpt_entry & 0x1) == 0 {
            return None; // Not present
        }

        // Check for 1GB huge page
        if (pdpt_entry & 0x80) != 0 {
            // 1GB huge page
            let page_base = pdpt_entry & 0x000f_ffff_c000_0000;
            let page_offset = virt_u64 & 0x3fff_ffff;
            return Some(PhysAddr::new(page_base + page_offset));
        }

        // Access PD
        let pd_addr = pdpt_entry & 0x000f_ffff_ffff_f000;
        let pd_ptr = pd_addr as *const u64;
        let pd_entry = core::ptr::read_volatile(pd_ptr.add(pd_idx));

        if (pd_entry & 0x1) == 0 {
            return None; // Not present
        }

        // Check for 2MB huge page
        if (pd_entry & 0x80) != 0 {
            // 2MB huge page
            let page_base = pd_entry & 0x000f_ffff_ffe0_0000;
            let page_offset = virt_u64 & 0x1f_ffff;
            return Some(PhysAddr::new(page_base + page_offset));
        }

        // Access PT
        let pt_addr = pd_entry & 0x000f_ffff_ffff_f000;
        let pt_ptr = pt_addr as *const u64;
        let pt_entry = core::ptr::read_volatile(pt_ptr.add(pt_idx));

        if (pt_entry & 0x1) == 0 {
            return None; // Not present
        }

        // 4KB page
        let page_base = pt_entry & 0x000f_ffff_ffff_f000;
        Some(PhysAddr::new(page_base + offset))
    }
}

/// Detect kernel's physical base address by walking BOOTBOOT page tables
///
/// This uses the current CR3 (BOOTBOOT's page tables) and translates the
/// kernel's virtual address to find where it was actually loaded.
///
/// # Safety
/// Must be called before switching away from BOOTBOOT's page tables.
pub unsafe fn detect_kernel_physical_base() -> Result<u64, &'static str> {
    unsafe {
        unsafe extern "C" {
            static __text_start: u8;
        }

        let kernel_virt = &__text_start as *const _ as u64;
        let current_cr3 = get_current_cr3();

        let kernel_phys = translate_via_identity(current_cr3, VirtAddr::new(kernel_virt))
            .ok_or("Failed to translate kernel virtual address")?;

        log::info!(
            "Detected kernel physical base: {:#x} (virt: {:#x})",
            kernel_phys.as_u64(),
            kernel_virt
        );

        Ok(kernel_phys.as_u64())
    }
}

/// Alias for map_page_user (backward compatibility)
pub fn map_user_page(
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let root = get_current_cr3();
    map_4k(root, virt, phys, flags)
}

/// Get kernel CR3 (alias for get_current_cr3)
pub fn get_kernel_cr3() -> PhysAddr {
    get_current_cr3()
}

/// Map multiple pages in batch (compatibility with old API)
///
/// Maps a batch of (VirtAddr, PhysAddr, PageTableFlags) tuples.
/// The kernel_cr3 parameter is ignored - we use the provided root.
pub fn map_pages_batch_in_table(
    root: PhysAddr,
    mappings: &[(VirtAddr, PhysAddr, PageTableFlags)],
    _kernel_cr3: Option<PhysAddr>,
) -> Result<(), &'static str> {
    for &(virt, phys, flags) in mappings {
        map_4k(root, virt, phys, flags)?;
    }
    Ok(())
}
