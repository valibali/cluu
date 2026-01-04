//! ELF64 Binary Loader
//!
//! This module implements an ELF64 (Executable and Linkable Format) loader
//! for loading userspace programs into CLUU.
//!
//! # ELF Format
//!
//! ELF binaries consist of:
//! - **ELF Header**: Magic number, architecture, entry point
//! - **Program Headers**: Describe segments to load (PT_LOAD)
//! - **Section Headers**: Describe sections (not needed for loading)
//! - **Data**: Actual code and data bytes
//!
//! # Loading Process (Bottom-Up)
//!
//! 1. Parse and validate ELF header
//! 2. Parse program headers (PT_LOAD segments)
//! 3. Allocate and map pages for each segment
//! 4. Copy segment data from ELF file
//! 5. Zero-fill BSS (uninitialized data)
//!
//! # Memory Layout After Loading
//!
//! - `0x0040_0000` - Text segment (code, read+execute)
//! - `0x0060_0000` - Data/BSS segment (data, read+write)
//! - `0x0080_0000` - Heap start (grows up via sbrk)
//! - `0x7ff0_0000` - Stack (grows down, 16MB)
//!
//! # Design Principles (SOLID)
//!
//! - **Single Responsibility**: ELF module only parses and loads binaries
//! - **Dependency Inversion**: Depends on AddressSpace abstraction, not concrete VMM
//! - **Open/Closed**: Can extend with new segment types without modifying core

use x86_64::{PhysAddr, VirtAddr};

// ═══════════════════════════════════════════════════════════════════════════
// ELF Constants and Magic Numbers
// ═══════════════════════════════════════════════════════════════════════════

/// ELF magic number (0x7F 'E' 'L' 'F')
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// ELF class (64-bit)
const ELFCLASS64: u8 = 2;

/// ELF data encoding (little-endian)
const ELFDATA2LSB: u8 = 1;

/// ELF version (current)
const EV_CURRENT: u8 = 1;

/// ELF type: Executable file
const ET_EXEC: u16 = 2;

/// ELF machine: AMD x86-64
const EM_X86_64: u16 = 62;

/// Program header type: Loadable segment
const PT_LOAD: u32 = 1;

/// Program header flags
const PF_X: u32 = 1; // Execute
const PF_W: u32 = 2; // Write
const PF_R: u32 = 4; // Read

// ═══════════════════════════════════════════════════════════════════════════
// ELF Structures
// ═══════════════════════════════════════════════════════════════════════════

/// ELF64 Header (64 bytes)
///
/// This is the first thing in an ELF file. It contains metadata about
/// the binary including architecture, entry point, and locations of
/// program/section headers.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct Elf64Header {
    e_ident: [u8; 16],   // ELF identification
    e_type: u16,         // Object file type
    e_machine: u16,      // Machine architecture
    e_version: u32,      // Object file version
    e_entry: u64,        // Entry point address
    e_phoff: u64,        // Program header offset
    e_shoff: u64,        // Section header offset
    e_flags: u32,        // Processor-specific flags
    e_ehsize: u16,       // ELF header size
    e_phentsize: u16,    // Program header entry size
    e_phnum: u16,        // Number of program headers
    e_shentsize: u16,    // Section header entry size
    e_shnum: u16,        // Number of section headers
    e_shstrndx: u16,     // Section header string table index
}

/// ELF64 Program Header (56 bytes)
///
/// Describes a segment of the program to be loaded into memory.
/// PT_LOAD segments tell us what to map and where.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct Elf64ProgramHeader {
    p_type: u32,    // Segment type
    p_flags: u32,   // Segment flags
    p_offset: u64,  // Segment file offset
    p_vaddr: u64,   // Segment virtual address
    p_paddr: u64,   // Segment physical address (ignored)
    p_filesz: u64,  // Segment size in file
    p_memsz: u64,   // Segment size in memory
    p_align: u64,   // Segment alignment
}

// ═══════════════════════════════════════════════════════════════════════════
// Error Types
// ═══════════════════════════════════════════════════════════════════════════

/// ELF loading errors
#[derive(Debug)]
pub enum ElfLoadError {
    InvalidMagic,
    InvalidClass,
    InvalidEncoding,
    InvalidVersion,
    InvalidType,
    InvalidMachine,
    InvalidHeader,
    NoLoadableSegments,
    SegmentTooLarge,
    MemoryAllocationFailed,
    MappingFailed(&'static str),
}

impl core::fmt::Display for ElfLoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ElfLoadError::InvalidMagic => write!(f, "Invalid ELF magic number"),
            ElfLoadError::InvalidClass => write!(f, "Not a 64-bit ELF"),
            ElfLoadError::InvalidEncoding => write!(f, "Not little-endian"),
            ElfLoadError::InvalidVersion => write!(f, "Invalid ELF version"),
            ElfLoadError::InvalidType => write!(f, "Not an executable"),
            ElfLoadError::InvalidMachine => write!(f, "Not an x86-64 binary"),
            ElfLoadError::InvalidHeader => write!(f, "Invalid ELF header"),
            ElfLoadError::NoLoadableSegments => write!(f, "No PT_LOAD segments"),
            ElfLoadError::SegmentTooLarge => write!(f, "Segment too large"),
            ElfLoadError::MemoryAllocationFailed => write!(f, "Failed to allocate memory"),
            ElfLoadError::MappingFailed(msg) => write!(f, "Failed to map pages: {}", msg),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ELF Header Parsing
// ═══════════════════════════════════════════════════════════════════════════

/// Parse and validate ELF header
///
/// Verifies:
/// - Magic number (0x7F 'E' 'L' 'F')
/// - 64-bit class
/// - Little-endian encoding
/// - Current version
/// - Executable type (ET_EXEC)
/// - x86-64 architecture
///
/// # Arguments
///
/// * `data` - Raw ELF file bytes
///
/// # Returns
///
/// Parsed header or error if validation fails
fn parse_elf_header(data: &[u8]) -> Result<Elf64Header, ElfLoadError> {
    // Verify minimum size
    if data.len() < core::mem::size_of::<Elf64Header>() {
        return Err(ElfLoadError::InvalidHeader);
    }

    // Parse header (careful with packed struct alignment)
    let header = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const Elf64Header) };

    // Validate magic number
    if header.e_ident[0..4] != ELF_MAGIC {
        klibcluu::error("ELF: Invalid magic");
        return Err(ElfLoadError::InvalidMagic);
    }

    // Validate class (64-bit)
    if header.e_ident[4] != ELFCLASS64 {
        klibcluu::error("ELF: Not 64-bit");
        return Err(ElfLoadError::InvalidClass);
    }

    // Validate encoding (little-endian)
    if header.e_ident[5] != ELFDATA2LSB {
        klibcluu::error("ELF: Not little-endian");
        return Err(ElfLoadError::InvalidEncoding);
    }

    // Validate version
    if header.e_ident[6] != EV_CURRENT {
        klibcluu::error("ELF: Invalid version");
        return Err(ElfLoadError::InvalidVersion);
    }

    // Read type and machine using read_unaligned (packed struct safety)
    let e_type = unsafe { core::ptr::addr_of!(header.e_type).read_unaligned() };
    let e_machine = unsafe { core::ptr::addr_of!(header.e_machine).read_unaligned() };
    let e_entry = unsafe { core::ptr::addr_of!(header.e_entry).read_unaligned() };

    // Validate type (executable)
    if e_type != ET_EXEC {
        klibcluu::error("ELF: Not executable");
        return Err(ElfLoadError::InvalidType);
    }

    // Validate machine (x86-64)
    if e_machine != EM_X86_64 {
        klibcluu::error("ELF: Not x86-64");
        return Err(ElfLoadError::InvalidMachine);
    }

    klibcluu::trace("ELF: Valid header, entry = 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", e_entry);
    Ok(header)
}

/// Parse program headers from ELF binary
///
/// Extracts the program header table which describes segments to load.
///
/// # Arguments
///
/// * `data` - Raw ELF file bytes
/// * `header` - Parsed ELF header
///
/// # Returns
///
/// Vector of program headers or error
fn parse_program_headers(
    data: &[u8],
    header: &Elf64Header,
) -> Result<alloc::vec::Vec<Elf64ProgramHeader>, ElfLoadError> {
    use alloc::vec::Vec;

    // Read fields using addr_of! for packed struct safety
    let ph_offset = unsafe { core::ptr::addr_of!(header.e_phoff).read_unaligned() as usize };
    let ph_size = unsafe { core::ptr::addr_of!(header.e_phentsize).read_unaligned() as usize };
    let ph_count = unsafe { core::ptr::addr_of!(header.e_phnum).read_unaligned() as usize };

    // Validate program header table bounds
    if ph_offset + (ph_size * ph_count) > data.len() {
        return Err(ElfLoadError::InvalidHeader);
    }

    let mut program_headers = Vec::new();

    for i in 0..ph_count {
        let offset = ph_offset + (i * ph_size);
        let ph_data = &data[offset..offset + ph_size];

        // Parse program header (careful with packed struct)
        let ph =
            unsafe { core::ptr::read_unaligned(ph_data.as_ptr() as *const Elf64ProgramHeader) };

        program_headers.push(ph);
    }

    Ok(program_headers)
}

/// Loaded ELF binary metadata
///
/// Contains information needed to start execution after loading.
#[derive(Debug)]
pub struct ElfBinary {
    /// Entry point (RIP for first thread)
    pub entry_point: VirtAddr,
}

/// Load an ELF binary into an address space
///
/// This is the main loading function. It:
/// 1. Parses the ELF header and program headers
/// 2. Maps pages for each PT_LOAD segment
/// 3. Copies segment data from the ELF file
/// 4. Zeros out BSS (uninitialized data)
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
    klibcluu::info("ELF: Loading binary (");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", data.len() as u64);
    klibcluu::info(" bytes)");

    // Parse and validate ELF header
    let header = parse_elf_header(data)?;
    let e_entry = unsafe { core::ptr::addr_of!(header.e_entry).read_unaligned() };
    let entry_point = VirtAddr::new(e_entry);

    klibcluu::info("ELF: Entry point at 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Info, "", entry_point.as_u64());

    // Parse program headers
    let program_headers = parse_program_headers(data, &header)?;
    klibcluu::info("ELF: Found ");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", program_headers.len() as u64);
    klibcluu::info(" program headers");

    let mut has_loadable = false;

    // Process each PT_LOAD segment
    for (i, ph) in program_headers.iter().enumerate() {
        // Read fields using addr_of! for packed struct safety
        let p_type = unsafe { core::ptr::addr_of!(ph.p_type).read_unaligned() };

        // Skip non-loadable segments
        if p_type != PT_LOAD {
            continue;
        }

        has_loadable = true;

        let p_vaddr = unsafe { core::ptr::addr_of!(ph.p_vaddr).read_unaligned() };
        let p_filesz = unsafe { core::ptr::addr_of!(ph.p_filesz).read_unaligned() };
        let p_memsz = unsafe { core::ptr::addr_of!(ph.p_memsz).read_unaligned() };
        let p_offset = unsafe { core::ptr::addr_of!(ph.p_offset).read_unaligned() };
        let p_flags = unsafe { core::ptr::addr_of!(ph.p_flags).read_unaligned() };

        let vaddr = VirtAddr::new(p_vaddr);
        let file_size = p_filesz as usize;
        let mem_size = p_memsz as usize;
        let file_offset = p_offset as usize;

        klibcluu::info("ELF: Segment ");
        klibcluu::log_dec(klibcluu::LogLevel::Info, "", i as u64);
        klibcluu::info(": vaddr=0x");
        klibcluu::log_hex(klibcluu::LogLevel::Info, "", p_vaddr);
        klibcluu::info(", filesz=");
        klibcluu::log_dec(klibcluu::LogLevel::Info, "", file_size as u64);
        klibcluu::info(", memsz=");
        klibcluu::log_dec(klibcluu::LogLevel::Info, "", mem_size as u64);

        // Validate segment bounds
        if file_offset + file_size > data.len() {
            klibcluu::error("ELF: Segment extends beyond file");
            return Err(ElfLoadError::InvalidHeader);
        }

        // Validate segment size (max 16MB per segment for safety)
        if mem_size > 16 * 1024 * 1024 {
            klibcluu::error("ELF: Segment too large");
            return Err(ElfLoadError::SegmentTooLarge);
        }

        // Load segment into memory
        unsafe {
            load_segment(
                address_space,
                vaddr,
                mem_size,
                &data[file_offset..file_offset + file_size],
                p_flags,
            )?;
        }
    }

    if !has_loadable {
        klibcluu::error("ELF: No loadable segments found");
        return Err(ElfLoadError::NoLoadableSegments);
    }

    klibcluu::info("ELF: Successfully loaded");

    Ok(ElfBinary { entry_point })
}

/// Load a segment into an address space
///
/// This function:
/// 1. Calculates page-aligned bounds
/// 2. Allocates physical frames for pages
/// 3. Maps pages into address space
/// 4. Copies data from ELF file via physmap
/// 5. Zeros BSS region (mem_size > file data length)
///
/// # Arguments
///
/// * `address_space` - Target address space
/// * `vaddr` - Virtual address to load at
/// * `mem_size` - Size in memory (including BSS)
/// * `file_data` - Data from ELF file
/// * `flags` - ELF segment flags (PF_R/PF_W/PF_X)
///
/// # Safety
///
/// Must be called after PMM and physmap are initialized
unsafe fn load_segment(
    address_space: &mut crate::mm::AddressSpace,
    vaddr: VirtAddr,
    mem_size: usize,
    file_data: &[u8],
    flags: u32,
) -> Result<(), ElfLoadError> {
    use core::ptr::write_bytes;

    // Calculate page-aligned bounds
    let start_page = vaddr.align_down(4096u64);
    let end_addr = vaddr + mem_size as u64;
    let end_page = end_addr.align_up(4096u64);
    let page_count = ((end_page - start_page) / 4096) as usize;

    klibcluu::trace("ELF:   Mapping ");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", page_count as u64);
    klibcluu::trace(" pages");

    // Determine writable and executable flags
    let writable = (flags & PF_W) != 0;
    let executable = (flags & PF_X) != 0;

    // Get page table root
    let page_table_root = address_space.page_table_root;

    // Allocate and map each page
    for page_idx in 0..page_count {
        let page_vaddr = start_page + (page_idx as u64 * 4096);

        // Allocate physical frame
        let phys_frame = crate::mm::pmm_simple::alloc_frame()
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;

        // Map page using VMM helper (similar to heap mapping)
        unsafe {
            map_user_page(page_vaddr.as_u64(), phys_frame, writable, executable, page_table_root)?;
        }

        // Zero the entire page via physmap
        let phys_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(phys_frame) };
        unsafe {
            write_bytes(phys_virt as *mut u8, 0, 4096);
        }
    }

    // Copy file data to mapped pages via physmap
    if !file_data.is_empty() {
        // Calculate which pages and offsets contain the data
        let data_start = vaddr.as_u64();

        let mut copied = 0;
        while copied < file_data.len() {
            let current_vaddr = data_start + copied as u64;
            let page_start = current_vaddr & !0xFFF;
            let offset_in_page = (current_vaddr & 0xFFF) as usize;
            let bytes_in_page = core::cmp::min(4096 - offset_in_page, file_data.len() - copied);

            // Translate virtual address to physical via page table walk
            // We need to do this because we're working with user page tables
            let phys_addr = translate_vaddr(page_table_root, VirtAddr::new(page_start))
                .ok_or(ElfLoadError::MappingFailed("Page not mapped"))?;

            // Convert physical to physmap virtual
            let phys_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(phys_addr.as_u64()) };

            // Copy data via physmap
            unsafe {
                let src = file_data.as_ptr().add(copied);
                let dst = (phys_virt as *mut u8).add(offset_in_page);
                core::ptr::copy_nonoverlapping(src, dst, bytes_in_page);
            }

            copied += bytes_in_page;
        }
    }

    // BSS is already zeroed (we zeroed all pages above)
    if mem_size > file_data.len() {
        let bss_size = mem_size - file_data.len();
        klibcluu::trace("ELF:   BSS: ");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", bss_size as u64);
        klibcluu::trace(" bytes (already zeroed)");
    }

    Ok(())
}

/// Map a single user page
///
/// Helper function to map a page into user address space with appropriate flags.
unsafe fn map_user_page(
    virt: u64,
    phys: u64,
    writable: bool,
    executable: bool,
    page_table_root: PhysAddr,
) -> Result<(), ElfLoadError> {
    use core::ptr::write_bytes;
    use crate::mm::vmm::pte_flags;

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
    let pml4_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64()) };
    let pml4 = unsafe { &mut *(pml4_virt as *mut [u64; 512]) };

    // Get or create PDPT
    let pdpt_phys = if pml4[pml4_idx] & 0x1 != 0 {
        pml4[pml4_idx] & !0xFFF
    } else {
        let pdpt_phys = crate::mm::pmm_simple::alloc_frame()
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pdpt_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(pdpt_phys) };
        unsafe { write_bytes(pdpt_virt as *mut u8, 0, 4096) };
        pml4[pml4_idx] = pdpt_phys | table_flags;
        pdpt_phys
    };

    let pdpt_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(pdpt_phys) };
    let pdpt = unsafe { &mut *(pdpt_virt as *mut [u64; 512]) };

    // Get or create PD
    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        pdpt[pdpt_idx] & !0xFFF
    } else {
        let pd_phys = crate::mm::pmm_simple::alloc_frame()
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pd_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(pd_phys) };
        unsafe { write_bytes(pd_virt as *mut u8, 0, 4096) };
        pdpt[pdpt_idx] = pd_phys | table_flags;
        pd_phys
    };

    let pd_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(pd_phys) };
    let pd = unsafe { &mut *(pd_virt as *mut [u64; 512]) };

    // Get or create PT
    let pt_phys = if pd[pd_idx] & 0x1 != 0 {
        pd[pd_idx] & !0xFFF
    } else {
        let pt_phys = crate::mm::pmm_simple::alloc_frame()
            .ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let pt_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(pt_phys) };
        unsafe { write_bytes(pt_virt as *mut u8, 0, 4096) };
        pd[pd_idx] = pt_phys | table_flags;
        pt_phys
    };

    let pt_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(pt_phys) };
    let pt = unsafe { &mut *(pt_virt as *mut [u64; 512]) };

    // Map the page
    if pt[pt_idx] & 0x1 != 0 {
        // Already mapped - this can happen when segments share pages
        return Ok(());
    }
    pt[pt_idx] = phys | page_flags;

    Ok(())
}

/// Translate virtual address to physical using page tables
///
/// Simplified version that only handles 4KB pages.
fn translate_vaddr(page_table_root: PhysAddr, vaddr: VirtAddr) -> Option<PhysAddr> {
    let pml4_idx = ((vaddr.as_u64() >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((vaddr.as_u64() >> 30) & 0x1FF) as usize;
    let pd_idx = ((vaddr.as_u64() >> 21) & 0x1FF) as usize;
    let pt_idx = ((vaddr.as_u64() >> 12) & 0x1FF) as usize;

    // Access PML4
    let pml4_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64()) };
    let pml4 = unsafe { &*(pml4_virt as *const [u64; 512]) };

    if pml4[pml4_idx] & 0x1 == 0 {
        return None;
    }

    // Access PDPT
    let pdpt_phys = pml4[pml4_idx] & !0xFFF;
    let pdpt_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(pdpt_phys) };
    let pdpt = unsafe { &*(pdpt_virt as *const [u64; 512]) };

    if pdpt[pdpt_idx] & 0x1 == 0 {
        return None;
    }

    // Access PD
    let pd_phys = pdpt[pdpt_idx] & !0xFFF;
    let pd_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(pd_phys) };
    let pd = unsafe { &*(pd_virt as *const [u64; 512]) };

    if pd[pd_idx] & 0x1 == 0 {
        return None;
    }

    // Access PT
    let pt_phys = pd[pd_idx] & !0xFFF;
    let pt_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(pt_phys) };
    let pt = unsafe { &*(pt_virt as *const [u64; 512]) };

    if pt[pt_idx] & 0x1 == 0 {
        return None;
    }

    // Get physical address
    let phys = pt[pt_idx] & !0xFFF;
    Some(PhysAddr::new(phys))
}

// ═══════════════════════════════════════════════════════════════════════════
// Unit Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a minimal valid ELF64 header for testing
    fn create_valid_elf_header() -> [u8; 64] {
        let mut header = [0u8; 64];

        // ELF Magic
        header[0..4].copy_from_slice(&ELF_MAGIC);

        // Class (64-bit)
        header[4] = ELFCLASS64;

        // Encoding (little-endian)
        header[5] = ELFDATA2LSB;

        // Version
        header[6] = EV_CURRENT;

        // Type (executable) - bytes 16-17 (little-endian)
        header[16] = (ET_EXEC & 0xFF) as u8;
        header[17] = ((ET_EXEC >> 8) & 0xFF) as u8;

        // Machine (x86-64) - bytes 18-19 (little-endian)
        header[18] = (EM_X86_64 & 0xFF) as u8;
        header[19] = ((EM_X86_64 >> 8) & 0xFF) as u8;

        // Version (again) - bytes 20-23 (little-endian u32)
        header[20] = 1;
        header[21] = 0;
        header[22] = 0;
        header[23] = 0;

        // Entry point - bytes 24-31 (little-endian u64)
        let entry = 0x400000u64;
        header[24..32].copy_from_slice(&entry.to_le_bytes());

        // Program header offset - bytes 32-39
        let phoff = 64u64;
        header[32..40].copy_from_slice(&phoff.to_le_bytes());

        // ELF header size - bytes 52-53
        let ehsize = 64u16;
        header[52..54].copy_from_slice(&ehsize.to_le_bytes());

        // Program header entry size - bytes 54-55
        let phentsize = 56u16;
        header[54..56].copy_from_slice(&phentsize.to_le_bytes());

        // Program header count - bytes 56-57
        let phnum = 0u16;
        header[56..58].copy_from_slice(&phnum.to_le_bytes());

        header
    }

    #[test]
    fn test_valid_elf_header() {
        let header_data = create_valid_elf_header();
        let result = parse_elf_header(&header_data);
        assert!(result.is_ok(), "Valid ELF header should parse successfully");

        let header = result.unwrap();
        let entry = unsafe { core::ptr::addr_of!(header.e_entry).read_unaligned() };
        assert_eq!(entry, 0x400000, "Entry point should be 0x400000");
    }

    #[test]
    fn test_invalid_magic() {
        let mut header_data = create_valid_elf_header();
        header_data[0] = 0xFF; // Corrupt magic

        let result = parse_elf_header(&header_data);
        assert!(matches!(result, Err(ElfLoadError::InvalidMagic)));
    }

    #[test]
    fn test_invalid_class() {
        let mut header_data = create_valid_elf_header();
        header_data[4] = 1; // 32-bit instead of 64-bit

        let result = parse_elf_header(&header_data);
        assert!(matches!(result, Err(ElfLoadError::InvalidClass)));
    }

    #[test]
    fn test_invalid_encoding() {
        let mut header_data = create_valid_elf_header();
        header_data[5] = 2; // Big-endian instead of little-endian

        let result = parse_elf_header(&header_data);
        assert!(matches!(result, Err(ElfLoadError::InvalidEncoding)));
    }

    #[test]
    fn test_invalid_version() {
        let mut header_data = create_valid_elf_header();
        header_data[6] = 0; // Invalid version

        let result = parse_elf_header(&header_data);
        assert!(matches!(result, Err(ElfLoadError::InvalidVersion)));
    }

    #[test]
    fn test_invalid_type() {
        let mut header_data = create_valid_elf_header();
        header_data[16] = 3; // DYN instead of EXEC
        header_data[17] = 0;

        let result = parse_elf_header(&header_data);
        assert!(matches!(result, Err(ElfLoadError::InvalidType)));
    }

    #[test]
    fn test_invalid_machine() {
        let mut header_data = create_valid_elf_header();
        header_data[18] = 3; // x86 (32-bit) instead of x86-64
        header_data[19] = 0;

        let result = parse_elf_header(&header_data);
        assert!(matches!(result, Err(ElfLoadError::InvalidMachine)));
    }

    #[test]
    fn test_too_small_header() {
        let header_data = [0u8; 32]; // Only 32 bytes instead of 64
        let result = parse_elf_header(&header_data);
        assert!(matches!(result, Err(ElfLoadError::InvalidHeader)));
    }

    #[test]
    fn test_parse_program_headers_empty() {
        let header_data = create_valid_elf_header();
        let header = parse_elf_header(&header_data).unwrap();

        // Create a minimal ELF file with just the header (no program headers)
        let elf_data = header_data.to_vec();

        let result = parse_program_headers(&elf_data, &header);
        assert!(result.is_ok(), "Should parse empty program header table");

        let program_headers = result.unwrap();
        assert_eq!(program_headers.len(), 0, "Should have 0 program headers");
    }

    #[test]
    fn test_parse_program_headers_with_data() {
        use alloc::vec::Vec;

        let mut header_data = create_valid_elf_header();

        // Set program header count to 1
        let phnum = 1u16;
        header_data[56..58].copy_from_slice(&phnum.to_le_bytes());

        let header = parse_elf_header(&header_data).unwrap();

        // Create ELF data with header + one program header
        let mut elf_data = Vec::from(header_data);

        // Add a PT_LOAD program header (56 bytes)
        let mut ph = [0u8; 56];
        ph[0..4].copy_from_slice(&PT_LOAD.to_le_bytes()); // p_type
        ph[4..8].copy_from_slice(&(PF_R | PF_X).to_le_bytes()); // p_flags (read+execute)
        ph[8..16].copy_from_slice(&0x1000u64.to_le_bytes()); // p_offset
        ph[16..24].copy_from_slice(&0x400000u64.to_le_bytes()); // p_vaddr
        ph[32..40].copy_from_slice(&0x1000u64.to_le_bytes()); // p_filesz
        ph[40..48].copy_from_slice(&0x1000u64.to_le_bytes()); // p_memsz

        elf_data.extend_from_slice(&ph);

        let result = parse_program_headers(&elf_data, &header);
        assert!(result.is_ok(), "Should parse program headers");

        let program_headers = result.unwrap();
        assert_eq!(program_headers.len(), 1, "Should have 1 program header");

        let ph = &program_headers[0];
        let p_type = unsafe { core::ptr::addr_of!(ph.p_type).read_unaligned() };
        assert_eq!(p_type, PT_LOAD, "Should be PT_LOAD");

        let p_vaddr = unsafe { core::ptr::addr_of!(ph.p_vaddr).read_unaligned() };
        assert_eq!(p_vaddr, 0x400000, "Virtual address should be 0x400000");
    }

    #[test]
    fn test_program_header_out_of_bounds() {
        let mut header_data = create_valid_elf_header();

        // Set program header count to 10 (but file is too small)
        let phnum = 10u16;
        header_data[56..58].copy_from_slice(&phnum.to_le_bytes());

        let header = parse_elf_header(&header_data).unwrap();

        // ELF data with header only (no room for 10 program headers)
        let elf_data = header_data.to_vec();

        let result = parse_program_headers(&elf_data, &header);
        assert!(matches!(result, Err(ElfLoadError::InvalidHeader)));
    }

    /// Test ELF flag to page flag conversion
    #[test]
    fn test_segment_flags() {
        // Read-only, executable (typical text segment)
        let text_flags = PF_R | PF_X;
        assert_eq!(text_flags, 5);

        // Read-write, non-executable (typical data segment)
        let data_flags = PF_R | PF_W;
        assert_eq!(data_flags, 6);

        // Read-write-execute (rare but valid)
        let rwx_flags = PF_R | PF_W | PF_X;
        assert_eq!(rwx_flags, 7);
    }

    /// Test that we can create an ElfBinary struct
    #[test]
    fn test_elf_binary_creation() {
        let entry_point = VirtAddr::new(0x400000);
        let binary = ElfBinary { entry_point };

        assert_eq!(binary.entry_point.as_u64(), 0x400000);
    }

    /// Test error display formatting
    #[test]
    fn test_error_display() {
        use alloc::format;

        assert_eq!(
            format!("{}", ElfLoadError::InvalidMagic),
            "Invalid ELF magic number"
        );
        assert_eq!(
            format!("{}", ElfLoadError::InvalidClass),
            "Not a 64-bit ELF"
        );
        assert_eq!(
            format!("{}", ElfLoadError::MemoryAllocationFailed),
            "Failed to allocate memory"
        );
    }
}

