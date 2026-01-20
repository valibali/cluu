//! ELF64 Binary Loader
//!
//! This module loads ELF binaries into address spaces. It uses klibcluu's
//! boot_elf module for parsing (single source of truth) and provides
//! kernel-internal batch mapping functions.
//!
//! # Loading Process
//!
//! 1. Parse ELF using klibcluu::boot_elf::ParsedElf
//! 2. For each PT_LOAD segment, batch-map pages
//! 3. Copy segment data from ELF file
//! 4. Zero-fill BSS (uninitialized data)
//!
//! # Memory Layout After Loading
//!
//! - `0x0040_0000` - Text segment (code, read+execute)
//! - `0x0060_0000` - Data/BSS segment (data, read+write)
//! - `0x0080_0000` - Heap start (grows up via sbrk)
//! - `0x7ff0_0000` - Stack (grows down, 16MB)

use crate::error::Error;
use crate::mm::vmm::pte_flags;
use klibcluu::boot_elf::{BootElfError, LoadableSegment, ParsedElf};
use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;
use x86_64::{PhysAddr, VirtAddr};

// ═══════════════════════════════════════════════════════════════════════════
// Error Types
// ═══════════════════════════════════════════════════════════════════════════

/// ELF loading errors
#[derive(Debug)]
pub enum ElfLoadError {
    ParseError(BootElfError),
    SegmentTooLarge,
    MemoryAllocationFailed,
    MappingFailed(&'static str),
    InvalidSegmentAddress,
    AddressConflict,
}

impl core::fmt::Display for ElfLoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ElfLoadError::ParseError(e) => write!(f, "ELF parse error: {:?}", e),
            ElfLoadError::SegmentTooLarge => write!(f, "Segment too large"),
            ElfLoadError::MemoryAllocationFailed => write!(f, "Failed to allocate memory"),
            ElfLoadError::MappingFailed(msg) => write!(f, "Failed to map pages: {}", msg),
            ElfLoadError::InvalidSegmentAddress => write!(f, "Invalid segment address"),
            ElfLoadError::AddressConflict => write!(f, "Address already mapped"),
        }
    }
}

impl From<BootElfError> for ElfLoadError {
    fn from(err: BootElfError) -> Self {
        ElfLoadError::ParseError(err)
    }
}

impl From<ElfLoadError> for Error {
    fn from(err: ElfLoadError) -> Self {
        match err {
            ElfLoadError::MemoryAllocationFailed => Error::OutOfMemory,
            ElfLoadError::MappingFailed(_) => Error::InvalidAddress,
            _ => Error::InvalidOperation,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Loaded Binary Metadata
// ═══════════════════════════════════════════════════════════════════════════

/// Loaded ELF binary metadata
#[derive(Debug)]
pub struct ElfBinary {
    /// Entry point (RIP for first thread)
    pub entry_point: VirtAddr,
}

// ═══════════════════════════════════════════════════════════════════════════
// Main Loading Function
// ═══════════════════════════════════════════════════════════════════════════

/// Load an ELF binary into an address space
///
/// Uses klibcluu::boot_elf for parsing and batch-maps segments.
///
/// # Arguments
///
/// * `data` - Raw ELF file bytes
/// * `address_space` - Target address space to load into
///
/// # Returns
///
/// Loaded binary metadata or error
pub fn load_elf(
    data: &[u8],
    address_space: &mut crate::mm::AddressSpace,
) -> Result<ElfBinary, ElfLoadError> {
    klibcluu::trace("ELF: Loading binary (");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, " bytes)", data.len() as u64);

    // Parse using klibcluu's shared parser
    let parsed = ParsedElf::parse(data)?;

    klibcluu::trace("ELF: Entry point at 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", parsed.entry_point);
    klibcluu::trace("ELF: Found ");
    klibcluu::log_dec(
        klibcluu::LogLevel::Trace,
        " loadable segments",
        parsed.segment_count as u64,
    );

    // Load each segment using batch mapping
    for segment in parsed.segments_iter() {
        load_segment_batch(address_space, segment, data)?;
    }

    klibcluu::info("ELF: Successfully loaded");

    Ok(ElfBinary {
        entry_point: VirtAddr::new(parsed.entry_point),
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Batch Segment Loading
// ═══════════════════════════════════════════════════════════════════════════

/// Load a segment using batch mapping
///
/// Allocates all pages for the segment, maps them in batch, then copies data.
fn load_segment_batch(
    address_space: &mut crate::mm::AddressSpace,
    segment: &LoadableSegment,
    elf_data: &[u8],
) -> Result<(), ElfLoadError> {
    use core::ptr::{copy_nonoverlapping, write_bytes};

    let vaddr = segment.vaddr;
    let mem_size = segment.mem_size as usize;
    let file_offset = segment.file_offset as usize;
    let file_size = segment.file_size as usize;

    // Validate segment size (max 16MB per segment for safety)
    if mem_size > 16 * 1024 * 1024 {
        klibcluu::error("ELF: Segment too large");
        return Err(ElfLoadError::SegmentTooLarge);
    }

    // Validate file bounds
    if file_offset + file_size > elf_data.len() {
        klibcluu::error("ELF: Segment extends beyond file");
        return Err(ElfLoadError::MappingFailed("segment out of bounds"));
    }

    let file_data = &elf_data[file_offset..file_offset + file_size];

    // Calculate page-aligned bounds
    let start_page = vaddr & !0xFFF;
    let end_addr = vaddr + mem_size as u64;
    let end_page = (end_addr + 0xFFF) & !0xFFF;
    let num_pages = ((end_page - start_page) / PAGE_SIZE as u64) as usize;

    klibcluu::trace("ELF: Segment vaddr=0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", vaddr);
    klibcluu::trace(" pages=");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", num_pages as u64);

    // Determine page flags from ELF segment flags
    let writable = segment.is_writable();
    let executable = segment.is_executable();
    let page_table_root = address_space.page_table_root;

    // Batch allocate and map all pages
    let mut bytes_copied = 0usize;
    let data_start_offset = (vaddr - start_page) as usize;

    for page_idx in 0..num_pages {
        let page_vaddr = start_page + (page_idx * PAGE_SIZE) as u64;

        // Allocate physical frame
        let frame_phys =
            crate::mm::pmm::alloc_frame().ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let frame_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(frame_phys) as *mut u8 };

        // Zero the entire page first
        unsafe {
            write_bytes(frame_virt, 0, PAGE_SIZE);
        }

        // Copy file data if this page contains any
        let page_start_in_segment = page_idx * PAGE_SIZE;
        let page_end_in_segment = page_start_in_segment + PAGE_SIZE;

        // Calculate overlap with file data region
        let file_data_start = data_start_offset;
        let file_data_end = data_start_offset + file_data.len();

        if page_end_in_segment > file_data_start && page_start_in_segment < file_data_end {
            // This page overlaps with file data
            let copy_start_in_page = file_data_start.saturating_sub(page_start_in_segment);

            let copy_start_in_file = page_start_in_segment.saturating_sub(file_data_start);

            let copy_end_in_page = if page_end_in_segment > file_data_end {
                file_data_end - page_start_in_segment
            } else {
                PAGE_SIZE
            };

            let copy_len = copy_end_in_page - copy_start_in_page;

            if copy_len > 0 && copy_start_in_file < file_data.len() {
                let actual_copy_len = copy_len.min(file_data.len() - copy_start_in_file);
                unsafe {
                    copy_nonoverlapping(
                        file_data.as_ptr().add(copy_start_in_file),
                        frame_virt.add(copy_start_in_page),
                        actual_copy_len,
                    );
                }
                bytes_copied += actual_copy_len;
            }
        }

        // Map the page
        unsafe {
            map_user_page(
                page_vaddr,
                frame_phys,
                writable,
                executable,
                page_table_root,
            )?;
        }
    }

    klibcluu::trace("ELF: Copied ");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, " bytes", bytes_copied as u64);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Page Mapping Functions
// ═══════════════════════════════════════════════════════════════════════════

/// Map a single 4KB user page
///
/// Helper function to map a page into user address space with appropriate flags.
pub(crate) unsafe fn map_user_page(
    virt: u64,
    phys: u64,
    writable: bool,
    executable: bool,
    page_table_root: PhysAddr,
) -> Result<(), ElfLoadError> {
    use core::ptr::write_bytes;

    // Calculate page table indices
    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx = ((virt >> 12) & 0x1FF) as usize;

    // Flags for intermediate tables (present + writable + user)
    let table_flags = pte_flags::PRESENT | pte_flags::WRITABLE | pte_flags::USER;

    // Flags for final PTE
    let mut page_flags = pte_flags::PRESENT | pte_flags::USER;
    if writable {
        page_flags |= pte_flags::WRITABLE;
    }
    if !executable {
        page_flags |= pte_flags::NO_EXECUTE;
    }

    // Access PML4
    let pml4_virt = crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64());
    let pml4 = &mut *(pml4_virt as *mut [u64; 512]);

    // Mask to extract physical address from PTE (bits 12-51 only)
    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    // Get or create PDPT
    let pdpt_phys = if pml4[pml4_idx] & 0x1 != 0 {
        pml4[pml4_idx] & PHYS_MASK
    } else {
        let pdpt_phys =
            crate::mm::pmm::alloc_frame().ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
        write_bytes(pdpt_virt as *mut u8, 0, 4096);
        pml4[pml4_idx] = pdpt_phys | table_flags;
        pdpt_phys
    };

    let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
    let pdpt = &mut *(pdpt_virt as *mut [u64; 512]);

    // Get or create PD
    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        pdpt[pdpt_idx] & PHYS_MASK
    } else {
        let pd_phys = crate::mm::pmm::alloc_frame().ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
        write_bytes(pd_virt as *mut u8, 0, 4096);
        pdpt[pdpt_idx] = pd_phys | table_flags;
        pd_phys
    };

    let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
    let pd = &mut *(pd_virt as *mut [u64; 512]);

    // Get or create PT
    let pt_phys = if pd[pd_idx] & 0x1 != 0 {
        pd[pd_idx] & PHYS_MASK
    } else {
        let pt_phys = crate::mm::pmm::alloc_frame().ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pt_virt = crate::mm::physmap::phys_to_virt_u64(pt_phys);
        write_bytes(pt_virt as *mut u8, 0, 4096);
        pd[pd_idx] = pt_phys | table_flags;
        pt_phys
    };

    let pt_virt = crate::mm::physmap::phys_to_virt_u64(pt_phys);
    let pt = &mut *(pt_virt as *mut [u64; 512]);

    // Map the page (overwrite any existing mapping)
    // Note: For ELF loading, segments may share pages so we allow remapping.
    // For space_grant, we need to replace the old mapping with the new one.
    pt[pt_idx] = phys | page_flags;

    // Flush TLB for this page to ensure CPU sees the new mapping
    core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));

    Ok(())
}

/// Map a 2MB large page into user address space
///
/// Both virtual and physical addresses must be 2MB-aligned.
/// Uses the PS (Page Size) bit in the Page Directory entry.
pub(crate) unsafe fn map_user_large_page(
    virt: u64,
    phys: u64,
    writable: bool,
    executable: bool,
    page_table_root: PhysAddr,
) -> Result<(), ElfLoadError> {
    use core::ptr::write_bytes;

    const LARGE_PAGE_SIZE: u64 = 2 * 1024 * 1024; // 2MB

    // Verify 2MB alignment
    if !virt.is_multiple_of(LARGE_PAGE_SIZE) || !phys.is_multiple_of(LARGE_PAGE_SIZE) {
        klibcluu::warn("map_user_large_page: addresses not 2MB aligned");
        return Err(ElfLoadError::InvalidSegmentAddress);
    }

    // Calculate page table indices (only PML4, PDPT, PD - no PT for large pages)
    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx = ((virt >> 21) & 0x1FF) as usize;

    // Flags for intermediate tables
    let table_flags = pte_flags::PRESENT | pte_flags::WRITABLE | pte_flags::USER;

    // Flags for the large page PDE (includes HUGE bit)
    let mut page_flags = pte_flags::PRESENT | pte_flags::USER | pte_flags::HUGE;
    if writable {
        page_flags |= pte_flags::WRITABLE;
    }
    if !executable {
        page_flags |= pte_flags::NO_EXECUTE;
    }

    // Access PML4
    let pml4_virt = crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64());
    let pml4 = &mut *(pml4_virt as *mut [u64; 512]);

    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    // Get or create PDPT
    let pdpt_phys = if pml4[pml4_idx] & 0x1 != 0 {
        pml4[pml4_idx] & PHYS_MASK
    } else {
        let pdpt_phys =
            crate::mm::pmm::alloc_frame().ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
        write_bytes(pdpt_virt as *mut u8, 0, 4096);
        pml4[pml4_idx] = pdpt_phys | table_flags;
        pdpt_phys
    };

    let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
    let pdpt = &mut *(pdpt_virt as *mut [u64; 512]);

    // Get or create PD
    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        pdpt[pdpt_idx] & PHYS_MASK
    } else {
        let pd_phys = crate::mm::pmm::alloc_frame().ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
        write_bytes(pd_virt as *mut u8, 0, 4096);
        pdpt[pdpt_idx] = pd_phys | table_flags;
        pd_phys
    };

    let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
    let pd = &mut *(pd_virt as *mut [u64; 512]);

    // Check if PD entry is already used
    if pd[pd_idx] & 0x1 != 0 {
        klibcluu::warn("map_user_large_page: PD entry already mapped");
        return Err(ElfLoadError::AddressConflict);
    }

    // Map the 2MB large page directly in PD (no PT needed)
    pd[pd_idx] = phys | page_flags;

    klibcluu::trace("Mapped 2MB large page: virt=0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, " phys=0x", virt);
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", phys);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Address Translation
// ═══════════════════════════════════════════════════════════════════════════

/// Translate a virtual address to physical using page tables
///
/// Walks the page table hierarchy to find the physical address.
pub fn translate_vaddr(page_table_root: PhysAddr, vaddr: VirtAddr) -> Option<PhysAddr> {
    let pml4_idx = ((vaddr.as_u64() >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((vaddr.as_u64() >> 30) & 0x1FF) as usize;
    let pd_idx = ((vaddr.as_u64() >> 21) & 0x1FF) as usize;
    let pt_idx = ((vaddr.as_u64() >> 12) & 0x1FF) as usize;
    let offset = vaddr.as_u64() & 0xFFF;

    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    unsafe {
        // PML4
        let pml4_virt = crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64());
        let pml4 = &*(pml4_virt as *const [u64; 512]);
        if pml4[pml4_idx] & 0x1 == 0 {
            return None;
        }

        // PDPT
        let pdpt_phys = pml4[pml4_idx] & PHYS_MASK;
        let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
        let pdpt = &*(pdpt_virt as *const [u64; 512]);
        if pdpt[pdpt_idx] & 0x1 == 0 {
            return None;
        }

        // Check for 1GB page
        if pdpt[pdpt_idx] & pte_flags::HUGE != 0 {
            let phys_base = pdpt[pdpt_idx] & 0x000F_FFFF_C000_0000;
            let gb_offset = vaddr.as_u64() & 0x3FFF_FFFF;
            return Some(PhysAddr::new(phys_base + gb_offset));
        }

        // PD
        let pd_phys = pdpt[pdpt_idx] & PHYS_MASK;
        let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
        let pd = &*(pd_virt as *const [u64; 512]);
        if pd[pd_idx] & 0x1 == 0 {
            return None;
        }

        // Check for 2MB page
        if pd[pd_idx] & pte_flags::HUGE != 0 {
            let phys_base = pd[pd_idx] & 0x000F_FFFF_FFE0_0000;
            let mb_offset = vaddr.as_u64() & 0x1F_FFFF;
            return Some(PhysAddr::new(phys_base + mb_offset));
        }

        // PT
        let pt_phys = pd[pd_idx] & PHYS_MASK;
        let pt_virt = crate::mm::physmap::phys_to_virt_u64(pt_phys);
        let pt = &*(pt_virt as *const [u64; 512]);
        if pt[pt_idx] & 0x1 == 0 {
            return None;
        }

        let page_phys = pt[pt_idx] & PHYS_MASK;
        Some(PhysAddr::new(page_phys + offset))
    }
}

/// Translate a virtual address to physical, also returning PTE flags
///
/// Returns (physical_address, pte_flags) or None if not mapped.
/// Used by userptr module to verify user page accessibility.
pub fn translate_vaddr_with_flags(
    page_table_root: PhysAddr,
    vaddr: VirtAddr,
) -> Option<(PhysAddr, u64)> {
    let pml4_idx = ((vaddr.as_u64() >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((vaddr.as_u64() >> 30) & 0x1FF) as usize;
    let pd_idx = ((vaddr.as_u64() >> 21) & 0x1FF) as usize;
    let pt_idx = ((vaddr.as_u64() >> 12) & 0x1FF) as usize;
    let offset = vaddr.as_u64() & 0xFFF;

    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    unsafe {
        // PML4
        let pml4_virt = crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64());
        let pml4 = &*(pml4_virt as *const [u64; 512]);
        if pml4[pml4_idx] & 0x1 == 0 {
            return None;
        }

        // PDPT
        let pdpt_phys = pml4[pml4_idx] & PHYS_MASK;
        let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
        let pdpt = &*(pdpt_virt as *const [u64; 512]);
        if pdpt[pdpt_idx] & 0x1 == 0 {
            return None;
        }

        // Check for 1GB page
        if pdpt[pdpt_idx] & pte_flags::HUGE != 0 {
            let phys_base = pdpt[pdpt_idx] & 0x000F_FFFF_C000_0000;
            let gb_offset = vaddr.as_u64() & 0x3FFF_FFFF;
            let flags = pdpt[pdpt_idx]
                & (pte_flags::PRESENT
                    | pte_flags::WRITABLE
                    | pte_flags::USER
                    | pte_flags::NO_EXECUTE
                    | pte_flags::HUGE);
            return Some((PhysAddr::new(phys_base + gb_offset), flags));
        }

        // PD
        let pd_phys = pdpt[pdpt_idx] & PHYS_MASK;
        let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
        let pd = &*(pd_virt as *const [u64; 512]);
        if pd[pd_idx] & 0x1 == 0 {
            return None;
        }

        // Check for 2MB page
        if pd[pd_idx] & pte_flags::HUGE != 0 {
            let phys_base = pd[pd_idx] & 0x000F_FFFF_FFE0_0000;
            let mb_offset = vaddr.as_u64() & 0x1F_FFFF;
            let flags = pd[pd_idx]
                & (pte_flags::PRESENT
                    | pte_flags::WRITABLE
                    | pte_flags::USER
                    | pte_flags::NO_EXECUTE
                    | pte_flags::HUGE);
            return Some((PhysAddr::new(phys_base + mb_offset), flags));
        }

        // PT
        let pt_phys = pd[pd_idx] & PHYS_MASK;
        let pt_virt = crate::mm::physmap::phys_to_virt_u64(pt_phys);
        let pt = &*(pt_virt as *const [u64; 512]);
        if pt[pt_idx] & 0x1 == 0 {
            return None;
        }

        let page_phys = pt[pt_idx] & PHYS_MASK;
        let flags = pt[pt_idx]
            & (pte_flags::PRESENT
                | pte_flags::WRITABLE
                | pte_flags::USER
                | pte_flags::NO_EXECUTE
                | pte_flags::HUGE);
        Some((PhysAddr::new(page_phys + offset), flags))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec;

    fn create_minimal_elf() -> alloc::vec::Vec<u8> {
        let mut elf = vec![0u8; 256];

        // ELF magic
        elf[0..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);

        // ELF identification
        elf[4] = 2; // ELFCLASS64
        elf[5] = 1; // ELFDATA2LSB
        elf[6] = 1; // EV_CURRENT

        // e_type = ET_EXEC
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());

        // e_machine = EM_X86_64
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());

        // e_version = 1
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());

        // e_entry = 0x400000
        elf[24..32].copy_from_slice(&0x400000u64.to_le_bytes());

        // e_phoff = 64
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());

        // e_ehsize = 64
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());

        // e_phentsize = 56
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());

        // e_phnum = 1
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());

        // Program header at offset 64
        // p_type = PT_LOAD
        elf[64..68].copy_from_slice(&1u32.to_le_bytes());

        // p_flags = PF_R | PF_X
        elf[68..72].copy_from_slice(&5u32.to_le_bytes());

        // p_vaddr = 0x400000
        elf[80..88].copy_from_slice(&0x400000u64.to_le_bytes());

        // p_filesz = 0x1000
        elf[96..104].copy_from_slice(&0x1000u64.to_le_bytes());

        // p_memsz = 0x1000
        elf[104..112].copy_from_slice(&0x1000u64.to_le_bytes());

        elf
    }

    #[test]
    fn test_parse_elf_via_klibcluu() {
        let elf_data = create_minimal_elf();
        let result = ParsedElf::parse(&elf_data);

        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.entry_point, 0x400000);
        assert_eq!(parsed.segment_count, 1);
    }

    #[test]
    fn test_segment_flags() {
        let elf_data = create_minimal_elf();
        let parsed = ParsedElf::parse(&elf_data).unwrap();
        let segment = parsed.get_segment(0).unwrap();

        assert!(segment.is_readable());
        assert!(segment.is_executable());
        assert!(!segment.is_writable());
    }
}
