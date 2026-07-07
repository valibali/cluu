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
/// * `owner` - `AddressSpaceId` of the target space; used to tag intermediate
///   page table frames with the correct owner. Pass `KERNEL_OWNER` for the
///   bootstrap / legacy spawn path where no real id has been assigned yet.
///   Never pass `AddressSpaceId::new(0)` — that is the old sentinel and is
///   indistinguishable from other zero-owner callers (Phase 2.5 fix).
///
/// # Returns
///
/// Loaded binary metadata or error
pub fn load_elf(
    data: &[u8],
    address_space: &mut crate::mm::AddressSpace,
    owner: crate::token::scope::AddressSpaceId,
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
        load_segment_batch(address_space, segment, data, owner)?;
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
    owner: crate::token::scope::AddressSpaceId,
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
        let frame_phys = crate::mm::pmm::alloc_frame_tagged("elf_alloc_leaf")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        // Phase 2.5: retype leaf frame as UserData with real owner. LOUD on error.
        if let Err(e) = crate::mm::frame_table::retype_to_user(frame_phys, owner) {
            #[cfg(debug_assertions)]
            panic!("load_segment_batch: retype_to_user phys=0x{:x} owner={} failed: {:?}",
                   frame_phys, owner.as_u64(), e);
            #[cfg(not(debug_assertions))]
            {
                klibcluu::error("load_segment_batch: retype_to_user failed — alias or double-alloc");
                klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", frame_phys);
                klibcluu::log_dec(klibcluu::LogLevel::Error, "  owner=", owner.as_u64());
                // Continue: the leaf frame is already zeroed; the PTE will be written.
                // The failure means the alias detector will fire later during teardown.
                let _ = e;
            }
        }
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
                owner,
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
/// `owner` is the `AddressSpaceId` of the target space; used to tag newly
/// allocated intermediate page tables with the correct owner.
pub(crate) unsafe fn map_user_page(
    virt: u64,
    phys: u64,
    writable: bool,
    executable: bool,
    page_table_root: PhysAddr,
    owner: crate::token::scope::AddressSpaceId,
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
        let pdpt_phys = crate::mm::pmm::alloc_frame_tagged("elf_alloc_pdpt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
        write_bytes(pdpt_virt as *mut u8, 0, 4096);
        pml4[pml4_idx] = (pdpt_phys & pte_flags::ADDR_MASK) | table_flags;
        // Phase 2.5: retype new PDPT with real owner. Errors are LOUD.
        if let Err(e) = crate::mm::frame_table::retype_to_pt(pdpt_phys, 3, owner) {
            #[cfg(debug_assertions)]
            panic!("map_user_page: retype PDPT 0x{:x} owner={} failed: {:?}",
                   pdpt_phys, owner.as_u64(), e);
            #[cfg(not(debug_assertions))]
            {
                klibcluu::error("map_user_page: retype PDPT failed — alias or double-alloc");
                klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", pdpt_phys);
                klibcluu::log_dec(klibcluu::LogLevel::Error, "  owner=", owner.as_u64());
                return Err(ElfLoadError::MappingFailed("retype_to_pt PDPT failed"));
            }
        }
        pdpt_phys
    };

    let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
    let pdpt = &mut *(pdpt_virt as *mut [u64; 512]);

    // Get or create PD
    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        // Reuse: verify no cross-space alias — the existing frame must belong
        // to the same owner (idempotent retype will succeed for same owner).
        let existing_pd_phys = pdpt[pdpt_idx] & PHYS_MASK;
        if let Err(e) = crate::mm::frame_table::retype_to_pt(existing_pd_phys, 2, owner) {
            klibcluu::error("map_user_page: reuse-PD retype failed — cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pd_phys=0x", existing_pd_phys);
            klibcluu::log_dec(klibcluu::LogLevel::Error, "  owner=", owner.as_u64());
            #[cfg(debug_assertions)]
            panic!("map_user_page: reuse PD cross-space alias: {:?}", e);
            #[cfg(not(debug_assertions))]
            { let _ = e; }
        }
        existing_pd_phys
    } else {
        let pd_phys = crate::mm::pmm::alloc_frame_tagged("elf_alloc_pd")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
        write_bytes(pd_virt as *mut u8, 0, 4096);
        pdpt[pdpt_idx] = (pd_phys & pte_flags::ADDR_MASK) | table_flags;
        // Phase 2.5: retype new PD with real owner. Errors are LOUD.
        if let Err(e) = crate::mm::frame_table::retype_to_pt(pd_phys, 2, owner) {
            #[cfg(debug_assertions)]
            panic!("map_user_page: retype PD 0x{:x} owner={} failed: {:?}",
                   pd_phys, owner.as_u64(), e);
            #[cfg(not(debug_assertions))]
            {
                klibcluu::error("map_user_page: retype PD failed — alias or double-alloc");
                klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", pd_phys);
                klibcluu::log_dec(klibcluu::LogLevel::Error, "  owner=", owner.as_u64());
                return Err(ElfLoadError::MappingFailed("retype_to_pt PD failed"));
            }
        }
        pd_phys
    };

    let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
    let pd = &mut *(pd_virt as *mut [u64; 512]);

    // Get or create PT
    let pt_phys = if pd[pd_idx] & 0x1 != 0 {
        // Reuse: verify no cross-space alias.
        let existing_pt_phys = pd[pd_idx] & PHYS_MASK;
        if let Err(e) = crate::mm::frame_table::retype_to_pt(existing_pt_phys, 1, owner) {
            klibcluu::error("map_user_page: reuse-PT retype failed — cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pt_phys=0x", existing_pt_phys);
            klibcluu::log_dec(klibcluu::LogLevel::Error, "  owner=", owner.as_u64());
            #[cfg(debug_assertions)]
            panic!("map_user_page: reuse PT cross-space alias: {:?}", e);
            #[cfg(not(debug_assertions))]
            { let _ = e; }
        }
        existing_pt_phys
    } else {
        let pt_phys = crate::mm::pmm::alloc_frame_tagged("elf_alloc_pt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pt_virt = crate::mm::physmap::phys_to_virt_u64(pt_phys);
        write_bytes(pt_virt as *mut u8, 0, 4096);
        pd[pd_idx] = (pt_phys & pte_flags::ADDR_MASK) | table_flags;
        // Phase 2.5: retype new PT with real owner. Errors are LOUD.
        if let Err(e) = crate::mm::frame_table::retype_to_pt(pt_phys, 1, owner) {
            #[cfg(debug_assertions)]
            panic!("map_user_page: retype PT 0x{:x} owner={} failed: {:?}",
                   pt_phys, owner.as_u64(), e);
            #[cfg(not(debug_assertions))]
            {
                klibcluu::error("map_user_page: retype PT failed — alias or double-alloc");
                klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", pt_phys);
                klibcluu::log_dec(klibcluu::LogLevel::Error, "  owner=", owner.as_u64());
                return Err(ElfLoadError::MappingFailed("retype_to_pt PT failed"));
            }
        }
        pt_phys
    };

    let pt_virt = crate::mm::physmap::phys_to_virt_u64(pt_phys);
    let pt = &mut *(pt_virt as *mut [u64; 512]);

    // Phase 2.6: if a present PTE already occupies this slot, dec_ref the old
    // physical frame before overwriting. This keeps the refcount in sync when
    // ELF segments share a page boundary or a remapping replaces an earlier
    // install. Skipping this would strand the old frame at refcount ≥ 1 with
    // no PTE, causing a permanent memory leak (UserData/Grant frames) or a
    // spurious PMM "still referenced" warning on the next retype attempt.
    // Device-mapped (NO_CACHE) and WC frames are tag=Device in the frame_table;
    // dec_ref no-ops silently for those.
    if pt[pt_idx] & pte_flags::PRESENT != 0 {
        let old_phys = pt[pt_idx] & PHYS_MASK;
        let _ = crate::mm::frame_table::dec_ref(old_phys);
    }

    // Map the page.
    // Mask phys to bits 12-51 so corrupted/garbage high bits never reach the
    // PTE's reserved range (52-62). A reserved bit set in a PTE produces a
    // RSV=1 page fault on the next walk, which we observed during the
    // compositor-swap session-handoff stress.
    pt[pt_idx] = (phys & pte_flags::ADDR_MASK) | page_flags;

    // Flush TLB for this page to ensure CPU sees the new mapping
    core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));

    Ok(())
}

/// Install a not-present guard PTE for a single 4KB virtual page.
///
/// Walks the page table hierarchy (allocating intermediate tables as needed)
/// but does NOT allocate a physical frame for the final PTE. The PTE is set
/// to `USER | NO_EXECUTE` with `PRESENT` clear, so any access faults. The
/// fault handler kills the thread or forwards to a registered fault_endpoint.
/// `teardown_user_pages` skips not-present PTEs, so no frame is freed on
/// space destruction.
pub(crate) unsafe fn map_guard_page(
    virt: u64,
    page_table_root: PhysAddr,
    owner: crate::token::scope::AddressSpaceId,
) -> Result<(), ElfLoadError> {
    use core::ptr::write_bytes;

    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx = ((virt >> 12) & 0x1FF) as usize;

    let table_flags = pte_flags::PRESENT | pte_flags::WRITABLE | pte_flags::USER;

    let pml4_virt = crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64());
    let pml4 = &mut *(pml4_virt as *mut [u64; 512]);

    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    let pdpt_phys = if pml4[pml4_idx] & 0x1 != 0 {
        pml4[pml4_idx] & PHYS_MASK
    } else {
        let pdpt_phys = crate::mm::pmm::alloc_frame_tagged("guard_pdpt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
        write_bytes(pdpt_virt as *mut u8, 0, 4096);
        pml4[pml4_idx] = (pdpt_phys & pte_flags::ADDR_MASK) | table_flags;
        if let Err(e) = crate::mm::frame_table::retype_to_pt(pdpt_phys, 3, owner) {
            #[cfg(debug_assertions)]
            panic!("map_guard_page: retype PDPT 0x{:x} owner={} failed: {:?}",
                   pdpt_phys, owner.as_u64(), e);
            #[cfg(not(debug_assertions))]
            {
                let _ = e;
                return Err(ElfLoadError::MappingFailed("retype_to_pt PDPT failed"));
            }
        }
        pdpt_phys
    };

    let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
    let pdpt = &mut *(pdpt_virt as *mut [u64; 512]);

    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        let existing_pd_phys = pdpt[pdpt_idx] & PHYS_MASK;
        if let Err(e) = crate::mm::frame_table::retype_to_pt(existing_pd_phys, 2, owner) {
            klibcluu::error("map_guard_page: reuse-PD retype failed — cross-space alias");
            #[cfg(debug_assertions)]
            panic!("map_guard_page: reuse PD cross-space alias: {:?}", e);
            #[cfg(not(debug_assertions))]
            { let _ = e; }
        }
        existing_pd_phys
    } else {
        let pd_phys = crate::mm::pmm::alloc_frame_tagged("guard_pd")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
        write_bytes(pd_virt as *mut u8, 0, 4096);
        pdpt[pdpt_idx] = (pd_phys & pte_flags::ADDR_MASK) | table_flags;
        if let Err(e) = crate::mm::frame_table::retype_to_pt(pd_phys, 2, owner) {
            #[cfg(debug_assertions)]
            panic!("map_guard_page: retype PD 0x{:x} owner={} failed: {:?}",
                   pd_phys, owner.as_u64(), e);
            #[cfg(not(debug_assertions))]
            {
                let _ = e;
                return Err(ElfLoadError::MappingFailed("retype_to_pt PD failed"));
            }
        }
        pd_phys
    };

    let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
    let pd = &mut *(pd_virt as *mut [u64; 512]);

    let pt_phys = if pd[pd_idx] & 0x1 != 0 {
        let existing_pt_phys = pd[pd_idx] & PHYS_MASK;
        if let Err(e) = crate::mm::frame_table::retype_to_pt(existing_pt_phys, 1, owner) {
            klibcluu::error("map_guard_page: reuse-PT retype failed — cross-space alias");
            #[cfg(debug_assertions)]
            panic!("map_guard_page: reuse PT cross-space alias: {:?}", e);
            #[cfg(not(debug_assertions))]
            { let _ = e; }
        }
        existing_pt_phys
    } else {
        let pt_phys = crate::mm::pmm::alloc_frame_tagged("guard_pt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pt_virt = crate::mm::physmap::phys_to_virt_u64(pt_phys);
        write_bytes(pt_virt as *mut u8, 0, 4096);
        pd[pd_idx] = (pt_phys & pte_flags::ADDR_MASK) | table_flags;
        if let Err(e) = crate::mm::frame_table::retype_to_pt(pt_phys, 1, owner) {
            #[cfg(debug_assertions)]
            panic!("map_guard_page: retype PT 0x{:x} owner={} failed: {:?}",
                   pt_phys, owner.as_u64(), e);
            #[cfg(not(debug_assertions))]
            {
                let _ = e;
                return Err(ElfLoadError::MappingFailed("retype_to_pt PT failed"));
            }
        }
        pt_phys
    };

    let pt_virt = crate::mm::physmap::phys_to_virt_u64(pt_phys);
    let pt = &mut *(pt_virt as *mut [u64; 512]);

    // Not-present PTE: USER set (intent: user-accessible), NO_EXECUTE set
    // (guard is never executable), PRESENT clear. Address field is 0 — no
    // physical frame is allocated for a guard page.
    pt[pt_idx] = pte_flags::USER | pte_flags::NO_EXECUTE;

    core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));

    Ok(())
}

/// Map a single 4KB shared physical frame into user address space.
///
/// Phase 2: installs a READ-ONLY PTE for `phys` and calls `inc_ref(phys)` to
/// record the new mapping in the typed-frame table. The SHARED_PHYS PTE bit is
/// retained for diagnostic tooling but `teardown_user_pages` no longer uses it
/// to skip PMM free — `dec_ref` handles that uniformly.
///
/// `owner` is the `AddressSpaceId` of the target space; used to tag any newly
/// allocated intermediate page tables.
pub(crate) unsafe fn map_shared_page(
    virt: u64,
    phys: u64,
    executable: bool,
    page_table_root: PhysAddr,
    owner: crate::token::scope::AddressSpaceId,
) -> Result<(), ElfLoadError> {
    use core::ptr::write_bytes;

    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx = ((virt >> 12) & 0x1FF) as usize;

    let table_flags = pte_flags::PRESENT | pte_flags::WRITABLE | pte_flags::USER;

    // Shared pages: PRESENT | USER | SHARED_PHYS (read-only, no WRITABLE).
    // SHARED_PHYS bit is kept as a diagnostic marker; teardown no longer gates on it.
    let mut page_flags = pte_flags::PRESENT | pte_flags::USER | pte_flags::SHARED_PHYS;
    if !executable {
        page_flags |= pte_flags::NO_EXECUTE;
    }

    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    let pml4_virt = crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64());
    let pml4 = &mut *(pml4_virt as *mut [u64; 512]);

    // map_shared_page: get or create PDPT with owner-check on reuse.
    let pdpt_phys = if pml4[pml4_idx] & 0x1 != 0 {
        let ep = pml4[pml4_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 3, owner).is_err() {
            klibcluu::error("map_shared_page: reuse-PDPT cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pdpt_phys=0x", ep);
        }
        ep
    } else {
        let p = crate::mm::pmm::alloc_frame_tagged("shared_alloc_pdpt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pml4[pml4_idx] = (p & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(p, 3, owner).is_err() {
            klibcluu::error("map_shared_page: retype PDPT failed");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", p);
            return Err(ElfLoadError::MappingFailed("retype_to_pt PDPT failed"));
        }
        p
    };

    let pdpt = &mut *(crate::mm::physmap::phys_to_virt_u64(pdpt_phys) as *mut [u64; 512]);

    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        let ep = pdpt[pdpt_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 2, owner).is_err() {
            klibcluu::error("map_shared_page: reuse-PD cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pd_phys=0x", ep);
        }
        ep
    } else {
        let p = crate::mm::pmm::alloc_frame_tagged("shared_alloc_pd")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pdpt[pdpt_idx] = (p & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(p, 2, owner).is_err() {
            klibcluu::error("map_shared_page: retype PD failed");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", p);
            return Err(ElfLoadError::MappingFailed("retype_to_pt PD failed"));
        }
        p
    };

    let pd = &mut *(crate::mm::physmap::phys_to_virt_u64(pd_phys) as *mut [u64; 512]);

    let pt_phys = if pd[pd_idx] & 0x1 != 0 {
        let ep = pd[pd_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 1, owner).is_err() {
            klibcluu::error("map_shared_page: reuse-PT cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pt_phys=0x", ep);
        }
        ep
    } else {
        let p = crate::mm::pmm::alloc_frame_tagged("shared_alloc_pt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pd[pd_idx] = (p & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(p, 1, owner).is_err() {
            klibcluu::error("map_shared_page: retype PT failed");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", p);
            return Err(ElfLoadError::MappingFailed("retype_to_pt PT failed"));
        }
        p
    };

    let pt = &mut *(crate::mm::physmap::phys_to_virt_u64(pt_phys) as *mut [u64; 512]);

    // Phase 2.6: if a present PTE already occupies this slot, dec_ref the old
    // physical frame before overwriting. This matches map_user_page's overwrite
    // path and keeps refcounts balanced when a shared (MAP_SHARE_PHYS) page is
    // remapped. Without this, the old frame's refcount stays elevated (leak)
    // while teardown later dec_refs the new frame — and if the old frame was
    // already at refcount 0, the asymmetry surfaces as spurious "dec_ref on
    // refcount=0" warnings.
    if pt[pt_idx] & pte_flags::PRESENT != 0 {
        let old_phys = pt[pt_idx] & PHYS_MASK;
        let _ = crate::mm::frame_table::dec_ref(old_phys);
    }

    // Mask phys to bits 12-51 — see comment in map_user_page (elf.rs:~335).
    pt[pt_idx] = (phys & pte_flags::ADDR_MASK) | page_flags;
    // Phase 2: inc_ref records this mapping in the typed-frame table.
    // inc_ref auto-transitions UserData → Grant when this is a second mapping.
    let _ = crate::mm::frame_table::inc_ref(phys);

    core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));

    Ok(())
}

/// Map a single device/MMIO page into user address space with cache disabled.
///
/// Sets PCD (bit 4) on the PTE, marking the page as uncacheable. This is
/// required for MMIO device registers and enables `teardown_user_pages()` to
/// identify and skip these frames during cleanup (NO_CACHE detection).
///
/// Device frames (tag=Device) are never managed by the refcount system —
/// no `inc_ref` is called here.
///
/// `owner` is the `AddressSpaceId` of the target space; used to tag any
/// newly allocated intermediate page tables.
pub(crate) unsafe fn map_device_page(
    virt: u64,
    phys: u64,
    writable: bool,
    page_table_root: PhysAddr,
    owner: crate::token::scope::AddressSpaceId,
) -> Result<(), ElfLoadError> {
    use crate::mm::vmm::pte_flags;
    use core::ptr::write_bytes;

    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx = ((virt >> 12) & 0x1FF) as usize;

    let table_flags = pte_flags::PRESENT | pte_flags::WRITABLE | pte_flags::USER;

    // Device pages: PRESENT | USER | NO_CACHE | NO_EXECUTE, optionally WRITABLE
    let mut page_flags =
        pte_flags::PRESENT | pte_flags::USER | pte_flags::NO_CACHE | pte_flags::NO_EXECUTE;
    if writable {
        page_flags |= pte_flags::WRITABLE;
    }

    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    let pml4_virt = crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64());
    let pml4 = &mut *(pml4_virt as *mut [u64; 512]);

    // map_device_page: get or create PDPT with owner-check on reuse.
    let pdpt_phys = if pml4[pml4_idx] & 0x1 != 0 {
        let ep = pml4[pml4_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 3, owner).is_err() {
            klibcluu::error("map_device_page: reuse-PDPT cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pdpt_phys=0x", ep);
        }
        ep
    } else {
        let p = crate::mm::pmm::alloc_frame_tagged("device_alloc_pdpt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pml4[pml4_idx] = (p & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(p, 3, owner).is_err() {
            klibcluu::error("map_device_page: retype PDPT failed");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", p);
            return Err(ElfLoadError::MappingFailed("retype_to_pt PDPT failed"));
        }
        p
    };

    let pdpt = &mut *(crate::mm::physmap::phys_to_virt_u64(pdpt_phys) as *mut [u64; 512]);

    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        let ep = pdpt[pdpt_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 2, owner).is_err() {
            klibcluu::error("map_device_page: reuse-PD cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pd_phys=0x", ep);
        }
        ep
    } else {
        let p = crate::mm::pmm::alloc_frame_tagged("device_alloc_pd")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pdpt[pdpt_idx] = (p & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(p, 2, owner).is_err() {
            klibcluu::error("map_device_page: retype PD failed");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", p);
            return Err(ElfLoadError::MappingFailed("retype_to_pt PD failed"));
        }
        p
    };

    let pd = &mut *(crate::mm::physmap::phys_to_virt_u64(pd_phys) as *mut [u64; 512]);

    let pt_phys = if pd[pd_idx] & 0x1 != 0 {
        let ep = pd[pd_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 1, owner).is_err() {
            klibcluu::error("map_device_page: reuse-PT cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pt_phys=0x", ep);
        }
        ep
    } else {
        let p = crate::mm::pmm::alloc_frame_tagged("device_alloc_pt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pd[pd_idx] = (p & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(p, 1, owner).is_err() {
            klibcluu::error("map_device_page: retype PT failed");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", p);
            return Err(ElfLoadError::MappingFailed("retype_to_pt PT failed"));
        }
        p
    };

    let pt = &mut *(crate::mm::physmap::phys_to_virt_u64(pt_phys) as *mut [u64; 512]);

    // Mask phys to bits 12-51 — see comment in map_user_page (elf.rs:~335).
    pt[pt_idx] = (phys & pte_flags::ADDR_MASK) | page_flags;
    // NOTE: Device frames are tagged Device in the frame_table; inc_ref is NOT
    // called here because device MMIO pages are never managed by the buddy PMM.

    core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));

    Ok(())
}

/// Map a single MMIO page write-combining (WC).
///
/// PTE bits: PRESENT | USER | PWT | NO_EXECUTE | SHARED_PHYS [+ WRITABLE].
/// PWT alone (PCD=0, PWT=1, PAT=0) selects PAT[1] = WC, configured by
/// `mm::pat::init()` at boot.
///
/// SHARED_PHYS is retained as a diagnostic marker. Like `map_device_page`,
/// no `inc_ref` is called — WC device frames are never managed by the buddy PMM.
///
/// `owner` is the `AddressSpaceId` of the target space.
pub(crate) unsafe fn map_device_page_wc(
    virt: u64,
    phys: u64,
    writable: bool,
    page_table_root: PhysAddr,
    owner: crate::token::scope::AddressSpaceId,
) -> Result<(), ElfLoadError> {
    use crate::mm::vmm::pte_flags;
    use core::ptr::write_bytes;

    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx = ((virt >> 12) & 0x1FF) as usize;

    let table_flags = pte_flags::PRESENT | pte_flags::WRITABLE | pte_flags::USER;

    // WC pages: PRESENT | USER | WRITE_COMBINING(PWT) | NO_EXECUTE | SHARED_PHYS, optionally WRITABLE.
    // Do NOT OR in NO_CACHE — that would flip PCD and select PAT[3] (UC), not PAT[1] (WC).
    let mut page_flags = pte_flags::PRESENT
        | pte_flags::USER
        | pte_flags::WRITE_COMBINING
        | pte_flags::NO_EXECUTE
        | pte_flags::SHARED_PHYS;
    if writable {
        page_flags |= pte_flags::WRITABLE;
    }

    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    let pml4_virt = crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64());
    let pml4 = &mut *(pml4_virt as *mut [u64; 512]);

    // map_device_page_wc: get or create PDPT with owner-check on reuse.
    let pdpt_phys = if pml4[pml4_idx] & 0x1 != 0 {
        let ep = pml4[pml4_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 3, owner).is_err() {
            klibcluu::error("map_device_page_wc: reuse-PDPT cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pdpt_phys=0x", ep);
        }
        ep
    } else {
        let p = crate::mm::pmm::alloc_frame_tagged("wc_alloc_pdpt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pml4[pml4_idx] = (p & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(p, 3, owner).is_err() {
            klibcluu::error("map_device_page_wc: retype PDPT failed");
            return Err(ElfLoadError::MappingFailed("retype_to_pt PDPT failed"));
        }
        p
    };

    let pdpt = &mut *(crate::mm::physmap::phys_to_virt_u64(pdpt_phys) as *mut [u64; 512]);

    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        let ep = pdpt[pdpt_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 2, owner).is_err() {
            klibcluu::error("map_device_page_wc: reuse-PD cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pd_phys=0x", ep);
        }
        ep
    } else {
        let p = crate::mm::pmm::alloc_frame_tagged("wc_alloc_pd")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pdpt[pdpt_idx] = (p & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(p, 2, owner).is_err() {
            klibcluu::error("map_device_page_wc: retype PD failed");
            return Err(ElfLoadError::MappingFailed("retype_to_pt PD failed"));
        }
        p
    };

    let pd = &mut *(crate::mm::physmap::phys_to_virt_u64(pd_phys) as *mut [u64; 512]);

    let pt_phys = if pd[pd_idx] & 0x1 != 0 {
        let ep = pd[pd_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 1, owner).is_err() {
            klibcluu::error("map_device_page_wc: reuse-PT cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pt_phys=0x", ep);
        }
        ep
    } else {
        let p = crate::mm::pmm::alloc_frame_tagged("wc_alloc_pt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pd[pd_idx] = (p & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(p, 1, owner).is_err() {
            klibcluu::error("map_device_page_wc: retype PT failed");
            return Err(ElfLoadError::MappingFailed("retype_to_pt PT failed"));
        }
        p
    };

    let pt = &mut *(crate::mm::physmap::phys_to_virt_u64(pt_phys) as *mut [u64; 512]);
    // Mask phys to bits 12-51 — see comment in map_user_page (elf.rs:~335).
    pt[pt_idx] = (phys & pte_flags::ADDR_MASK) | page_flags;
    // NOTE: WC device frames are not PMM-managed; inc_ref is NOT called.

    core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));

    Ok(())
}

/// Map a 2MB large page into user address space
///
/// Both virtual and physical addresses must be 2MB-aligned.
/// Uses the PS (Page Size) bit in the Page Directory entry.
///
/// `owner` is the `AddressSpaceId` of the target space; used to tag any
/// newly allocated intermediate page tables.
pub(crate) unsafe fn map_user_large_page(
    virt: u64,
    phys: u64,
    writable: bool,
    executable: bool,
    page_table_root: PhysAddr,
    owner: crate::token::scope::AddressSpaceId,
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

    // map_user_large_page: get or create PDPT with owner-check on reuse.
    let pdpt_phys = if pml4[pml4_idx] & 0x1 != 0 {
        let ep = pml4[pml4_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 3, owner).is_err() {
            klibcluu::error("map_user_large_page: reuse-PDPT cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pdpt_phys=0x", ep);
        }
        ep
    } else {
        let pdpt_phys = crate::mm::pmm::alloc_frame_tagged("large_alloc_pdpt")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
        write_bytes(pdpt_virt as *mut u8, 0, 4096);
        pml4[pml4_idx] = (pdpt_phys & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(pdpt_phys, 3, owner).is_err() {
            klibcluu::error("map_user_large_page: retype PDPT failed");
            return Err(ElfLoadError::MappingFailed("retype_to_pt PDPT failed"));
        }
        pdpt_phys
    };

    let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
    let pdpt = &mut *(pdpt_virt as *mut [u64; 512]);

    // Get or create PD
    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        let ep = pdpt[pdpt_idx] & PHYS_MASK;
        if crate::mm::frame_table::retype_to_pt(ep, 2, owner).is_err() {
            klibcluu::error("map_user_large_page: reuse-PD cross-space alias");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  pd_phys=0x", ep);
        }
        ep
    } else {
        let pd_phys = crate::mm::pmm::alloc_frame_tagged("large_alloc_pd")
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
        write_bytes(pd_virt as *mut u8, 0, 4096);
        pdpt[pdpt_idx] = (pd_phys & pte_flags::ADDR_MASK) | table_flags;
        if crate::mm::frame_table::retype_to_pt(pd_phys, 2, owner).is_err() {
            klibcluu::error("map_user_large_page: retype PD failed");
            return Err(ElfLoadError::MappingFailed("retype_to_pt PD failed"));
        }
        pd_phys
    };

    let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
    let pd = &mut *(pd_virt as *mut [u64; 512]);

    // Check if PD entry is already used
    if pd[pd_idx] & 0x1 != 0 {
        klibcluu::warn("map_user_large_page: PD entry already mapped");
        return Err(ElfLoadError::AddressConflict);
    }

    // Map the 2MB large page directly in PD (no PT needed).
    // Mask phys to bits 21-51 — bits 13-20 are reserved MBZ for 2 MiB
    // huge-page PDEs. A reserved bit set yields a RSV=1 fault on access.
    pd[pd_idx] = (phys & pte_flags::HUGE_ADDR_MASK) | page_flags;

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

/// Diagnostic: scan a user page table for the *first* 4 KiB virtual page that
/// maps `target_phys`. Returns the user virtual address of that page, or None.
///
/// Used by the wild-jump PF diagnostic to confirm whether two address spaces
/// alias the same physical frame (suspected MAP_SHARE_PHYS / cache aliasing
/// bug). Walks PML4 user entries (0..256), skips 1 GiB and 2 MiB huge mappings
/// (the kernel's userspace doesn't use them for normal data), only descends
/// into present non-huge entries.
///
/// O(N) where N is the number of present 4 KiB user pages — sparse spaces
/// finish quickly. Returns the first match (lowest VA).
pub fn find_first_va_for_phys(page_table_root: PhysAddr, target_phys: u64) -> Option<u64> {
    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;
    let target = target_phys & PHYS_MASK;

    unsafe {
        let pml4_virt = crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64());
        let pml4 = &*(pml4_virt as *const [u64; 512]);

        for pml4_idx in 0..256usize {
            if pml4[pml4_idx] & 0x1 == 0 {
                continue;
            }
            let pdpt_phys = pml4[pml4_idx] & PHYS_MASK;
            let pdpt_virt = crate::mm::physmap::phys_to_virt_u64(pdpt_phys);
            let pdpt = &*(pdpt_virt as *const [u64; 512]);

            for pdpt_idx in 0..512usize {
                if pdpt[pdpt_idx] & 0x1 == 0 {
                    continue;
                }
                if pdpt[pdpt_idx] & pte_flags::HUGE != 0 {
                    continue; // skip 1 GiB pages
                }
                let pd_phys = pdpt[pdpt_idx] & PHYS_MASK;
                let pd_virt = crate::mm::physmap::phys_to_virt_u64(pd_phys);
                let pd = &*(pd_virt as *const [u64; 512]);

                for pd_idx in 0..512usize {
                    if pd[pd_idx] & 0x1 == 0 {
                        continue;
                    }
                    if pd[pd_idx] & pte_flags::HUGE != 0 {
                        continue; // skip 2 MiB pages
                    }
                    let pt_phys = pd[pd_idx] & PHYS_MASK;
                    let pt_virt = crate::mm::physmap::phys_to_virt_u64(pt_phys);
                    let pt = &*(pt_virt as *const [u64; 512]);

                    for pt_idx in 0..512usize {
                        if pt[pt_idx] & 0x1 == 0 {
                            continue;
                        }
                        let phys = pt[pt_idx] & PHYS_MASK;
                        if phys == target {
                            let va = ((pml4_idx as u64) << 39)
                                | ((pdpt_idx as u64) << 30)
                                | ((pd_idx as u64) << 21)
                                | ((pt_idx as u64) << 12);
                            return Some(va);
                        }
                    }
                }
            }
        }
    }
    None
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
