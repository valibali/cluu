/*
 * ELF Binary Loader
 *
 * This module implements an ELF64 (Executable and Linkable Format) loader
 * for loading userspace programs into CLUU.
 *
 * ELF Format:
 * ===========
 *
 * ELF binaries consist of:
 * - ELF Header: Magic number, architecture, entry point
 * - Program Headers: Describe segments to load (PT_LOAD)
 * - Section Headers: Describe sections (not needed for loading)
 * - Data: Actual code and data bytes
 *
 * Loading Process:
 * ================
 *
 * 1. Parse and validate ELF header
 * 2. Parse program headers (PT_LOAD segments)
 * 3. Create new process with fresh address space
 * 4. Map each PT_LOAD segment into process memory
 * 5. Copy segment data from ELF file
 * 6. Zero-fill BSS (uninitialized data)
 * 7. Set up user stack
 * 8. Create initial thread at entry point
 *
 * Memory Layout After Loading:
 * ============================
 *
 * 0x00400000 - Text segment (code, read+execute)
 * 0x00600000 - Data/BSS segment (data, read+write)
 * 0x00800000 - Heap start (grows up via sbrk)
 * 0x7ff00000 - Stack (grows down, 16MB)
 *
 * References:
 * - ELF64 Specification: https://refspecs.linuxfoundation.org/elf/elf.pdf
 * - System V ABI AMD64: https://refspecs.linuxfoundation.org/elf/x86_64-abi-0.99.pdf
 */

use alloc::vec::Vec;
use x86_64::{PhysAddr, VirtAddr};
use x86_64::structures::paging::PageTableFlags;

use crate::memory::{paging, phys, AddressSpace};
use crate::scheduler::{self, ProcessId, ThreadId};

/// Trampoline function for userspace entry
///
/// This function should never actually execute - it's just a placeholder.
/// The thread's interrupt context will be set up to jump directly to
/// userspace via IRETQ when the thread is first scheduled.
fn userspace_entry_trampoline() {
    log::warn!("userspace_entry_trampoline executed - this should not happen!");
    loop {
        crate::scheduler::yield_now();
    }
}

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

/// ELF64 Header (64 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct Elf64Header {
    e_ident: [u8; 16],      // ELF identification
    e_type: u16,            // Object file type
    e_machine: u16,         // Machine architecture
    e_version: u32,         // Object file version
    e_entry: u64,           // Entry point address
    e_phoff: u64,           // Program header offset
    e_shoff: u64,           // Section header offset
    e_flags: u32,           // Processor-specific flags
    e_ehsize: u16,          // ELF header size
    e_phentsize: u16,       // Program header entry size
    e_phnum: u16,           // Number of program headers
    e_shentsize: u16,       // Section header entry size
    e_shnum: u16,           // Number of section headers
    e_shstrndx: u16,        // Section header string table index
}

/// ELF64 Program Header (56 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct Elf64ProgramHeader {
    p_type: u32,       // Segment type
    p_flags: u32,      // Segment flags
    p_offset: u64,     // Segment file offset
    p_vaddr: u64,      // Segment virtual address
    p_paddr: u64,      // Segment physical address (ignored)
    p_filesz: u64,     // Segment size in file
    p_memsz: u64,      // Segment size in memory
    p_align: u64,      // Segment alignment
}

/// Loaded ELF binary metadata
#[derive(Debug)]
pub struct ElfBinary {
    /// Entry point (RIP for first thread)
    pub entry_point: VirtAddr,
    /// Loaded segments
    pub segments: Vec<ElfSegment>,
}

/// A loaded ELF segment
#[derive(Debug, Clone)]
pub struct ElfSegment {
    /// Virtual address where segment is loaded
    pub vaddr: VirtAddr,
    /// Size of segment in memory
    pub size: usize,
    /// Page table flags (derived from ELF flags)
    pub flags: PageTableFlags,
}

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
    InvalidAlignment,
    MemoryAllocationFailed,
    MappingFailed,
    ThreadSetupFailed,
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
            ElfLoadError::InvalidAlignment => write!(f, "Invalid segment alignment"),
            ElfLoadError::MemoryAllocationFailed => write!(f, "Failed to allocate memory"),
            ElfLoadError::MappingFailed => write!(f, "Failed to map pages"),
            ElfLoadError::ThreadSetupFailed => write!(f, "Failed to set up userspace thread"),
        }
    }
}

/// Parse and validate ELF header
///
/// Verifies:
/// - Magic number (0x7F 'E' 'L' 'F')
/// - 64-bit class
/// - Little-endian encoding
/// - Current version
/// - Executable type (ET_EXEC)
/// - x86-64 architecture
fn parse_elf_header(data: &[u8]) -> Result<Elf64Header, ElfLoadError> {
    // Verify minimum size
    if data.len() < core::mem::size_of::<Elf64Header>() {
        return Err(ElfLoadError::InvalidHeader);
    }

    // Parse header (careful with packed struct alignment)
    let header = unsafe {
        core::ptr::read_unaligned(data.as_ptr() as *const Elf64Header)
    };

    // Validate magic number
    if header.e_ident[0..4] != ELF_MAGIC {
        log::error!("ELF: Invalid magic: {:?}", &header.e_ident[0..4]);
        return Err(ElfLoadError::InvalidMagic);
    }

    // Validate class (64-bit)
    if header.e_ident[4] != ELFCLASS64 {
        log::error!("ELF: Not 64-bit (class = {})", header.e_ident[4]);
        return Err(ElfLoadError::InvalidClass);
    }

    // Validate encoding (little-endian)
    if header.e_ident[5] != ELFDATA2LSB {
        log::error!("ELF: Not little-endian (encoding = {})", header.e_ident[5]);
        return Err(ElfLoadError::InvalidEncoding);
    }

    // Validate version
    if header.e_ident[6] != EV_CURRENT {
        log::error!("ELF: Invalid version ({})", header.e_ident[6]);
        return Err(ElfLoadError::InvalidVersion);
    }

    // Read type and machine using read_unaligned (packed struct safety)
    let e_type = unsafe { core::ptr::addr_of!(header.e_type).read_unaligned() };
    let e_machine = unsafe { core::ptr::addr_of!(header.e_machine).read_unaligned() };
    let e_entry = unsafe { core::ptr::addr_of!(header.e_entry).read_unaligned() };

    // Validate type (executable)
    if e_type != ET_EXEC {
        log::error!("ELF: Not executable (type = {})", e_type);
        return Err(ElfLoadError::InvalidType);
    }

    // Validate machine (x86-64)
    if e_machine != EM_X86_64 {
        log::error!("ELF: Not x86-64 (machine = {})", e_machine);
        return Err(ElfLoadError::InvalidMachine);
    }

    log::debug!("ELF: Valid header, entry = 0x{:x}", e_entry);
    Ok(header)
}

/// Parse program headers from ELF binary
fn parse_program_headers(
    data: &[u8],
    header: &Elf64Header,
) -> Result<Vec<Elf64ProgramHeader>, ElfLoadError> {
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
        let ph = unsafe {
            core::ptr::read_unaligned(ph_data.as_ptr() as *const Elf64ProgramHeader)
        };

        program_headers.push(ph);
    }

    Ok(program_headers)
}

/// Convert ELF segment flags to page table flags
fn elf_flags_to_page_flags(elf_flags: u32) -> PageTableFlags {
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;

    // Write permission
    if (elf_flags & PF_W) != 0 {
        flags |= PageTableFlags::WRITABLE;
    }

    // Execute permission (note: x86-64 has NXE - No-Execute Enable)
    // If segment is NOT executable, we would set NO_EXECUTE
    // For now, we'll keep it simple and allow execution on all pages
    // TODO: Use NO_EXECUTE flag when PF_X is not set

    flags
}

/// Load an ELF binary into a new process's address space
///
/// This function:
/// 1. Parses the ELF header and program headers
/// 2. Creates mappings for each PT_LOAD segment
/// 3. Copies segment data from the ELF file
/// 4. Zeros out BSS (uninitialized data)
/// 5. Returns metadata about the loaded binary
///
/// The address space must already be initialized and activated.
///
/// # Arguments
/// * `kernel_cr3` - Optional kernel CR3 to avoid deadlock when called within scheduler lock
pub fn load_elf_binary(
    data: &[u8],
    address_space: &mut AddressSpace,
    kernel_cr3: Option<PhysAddr>,
) -> Result<ElfBinary, ElfLoadError> {
    log::info!("ELF: Loading binary ({} bytes)", data.len());

    // Parse and validate ELF header
    let header = parse_elf_header(data)?;
    let e_entry = unsafe { core::ptr::addr_of!(header.e_entry).read_unaligned() };
    let entry_point = VirtAddr::new(e_entry);
    log::info!("ELF: Entry point at 0x{:x}", entry_point.as_u64());

    // Parse program headers
    let program_headers = parse_program_headers(data, &header)?;
    log::info!("ELF: Found {} program headers", program_headers.len());

    // Get the page table root for mapping
    let page_table_root = address_space.page_table_root;
    log::debug!("ELF: Mapping into page table at {:#x}", page_table_root.as_u64());

    let mut segments = Vec::new();
    let mut has_loadable = false;

    // Process each PT_LOAD segment
    for (i, ph) in program_headers.iter().enumerate() {
        // Read fields using addr_of! for packed struct safety
        let p_type = unsafe { core::ptr::addr_of!(ph.p_type).read_unaligned() };
        let p_vaddr = unsafe { core::ptr::addr_of!(ph.p_vaddr).read_unaligned() };
        let p_filesz = unsafe { core::ptr::addr_of!(ph.p_filesz).read_unaligned() };
        let p_memsz = unsafe { core::ptr::addr_of!(ph.p_memsz).read_unaligned() };
        let p_offset = unsafe { core::ptr::addr_of!(ph.p_offset).read_unaligned() };
        let p_flags = unsafe { core::ptr::addr_of!(ph.p_flags).read_unaligned() };

        // Skip non-loadable segments
        if p_type != PT_LOAD {
            log::debug!("ELF: Segment {}: type={}, skipping", i, p_type);
            continue;
        }

        has_loadable = true;

        let vaddr = VirtAddr::new(p_vaddr);
        let file_size = p_filesz as usize;
        let mem_size = p_memsz as usize;
        let file_offset = p_offset as usize;

        log::info!(
            "ELF: Segment {}: vaddr=0x{:x}, filesz={}, memsz={}, flags=0x{:x}",
            i, p_vaddr, file_size, mem_size, p_flags
        );

        // Validate segment bounds
        if file_offset + file_size > data.len() {
            log::error!("ELF: Segment {} extends beyond file", i);
            return Err(ElfLoadError::InvalidHeader);
        }

        // Validate segment size (max 16MB per segment for safety)
        if mem_size > 16 * 1024 * 1024 {
            log::error!("ELF: Segment {} too large ({})", i, mem_size);
            return Err(ElfLoadError::SegmentTooLarge);
        }

        // Convert ELF flags to page flags
        let flags = elf_flags_to_page_flags(ph.p_flags);

        // Calculate page-aligned bounds
        let start_page = vaddr.align_down(4096u64);
        let end_page = (vaddr + mem_size as u64).align_up(4096u64);
        let page_count = ((end_page - start_page) / 4096) as usize;

        log::debug!(
            "ELF:   Mapping {} pages from 0x{:x} to 0x{:x}",
            page_count,
            start_page.as_u64(),
            end_page.as_u64()
        );

        // Allocate and map pages for this segment using batch operation
        let user_flags = flags | PageTableFlags::USER_ACCESSIBLE;
        let mut mappings = alloc::vec::Vec::with_capacity(page_count);

        for page_idx in 0..page_count {
            let page_vaddr = start_page + (page_idx as u64 * 4096);

            // Allocate physical frame
            let frame = phys::alloc_frame()
                .ok_or(ElfLoadError::MemoryAllocationFailed)?;
            let phys_addr = PhysAddr::new(frame.start_address());

            log::debug!(
                "ELF:   Preparing page: vaddr=0x{:x}, phys=0x{:x}, flags={:?}",
                page_vaddr.as_u64(),
                phys_addr.as_u64(),
                user_flags
            );

            // Add to batch
            mappings.push((page_vaddr, phys_addr, user_flags));
        }

        // Map all segment pages in a single batch
        if let Err(e) = paging::map_pages_batch_in_table(page_table_root, &mappings, kernel_cr3) {
            log::error!("ELF:   Segment mapping failed: {:?}", e);
            log::error!("ELF:     segment: {}", i);
            log::error!("ELF:     page_count: {}", page_count);
            log::error!("ELF:     page_table_root: 0x{:x}", page_table_root.as_u64());
            return Err(ElfLoadError::MappingFailed);
        }

        // CRITICAL DEBUG: Verify code segment mapping for _start
        if vaddr.as_u64() <= 0x400b40 && (vaddr.as_u64() + mem_size as u64) > 0x400b40 {
            let entry_offset = 0x400b40 - vaddr.as_u64();
            let entry_page_idx = (entry_offset / 4096) as usize;
            if entry_page_idx < mappings.len() {
                log::info!("ELF:   _start (0x400b40) is in segment {} at offset 0x{:x}",
                           i, entry_offset);
                log::info!("ELF:   _start page: virt=0x{:x} -> phys=0x{:x}",
                           mappings[entry_page_idx].0.as_u64(),
                           mappings[entry_page_idx].1.as_u64());
            }
        }

        // Copy ELF data to kernel buffer BEFORE switching page tables
        // We do this because initrd is not mapped in userspace page table
        let kernel_buffer = if file_size > 0 {
            let mut buf = alloc::vec::Vec::with_capacity(file_size);
            buf.extend_from_slice(&data[file_offset..file_offset + file_size]);
            Some(buf)
        } else {
            None
        };

        // Switch to userspace page table to zero pages and copy data
        // We must do this because the pages are only mapped in the userspace page table
        use x86_64::registers::control::Cr3;
        use x86_64::structures::paging::PhysFrame;
        let (old_cr3, cr3_flags) = Cr3::read();
        let new_cr3 = PhysFrame::containing_address(page_table_root);
        unsafe { Cr3::write(new_cr3, cr3_flags); }

        // Zero all pages in this segment (needed for BSS and security)
        unsafe {
            let segment_ptr = start_page.as_mut_ptr::<u8>();
            core::ptr::write_bytes(segment_ptr, 0, page_count * 4096);
        }

        // Copy segment data from kernel buffer to userspace memory
        if let Some(ref buf) = kernel_buffer {
            unsafe {
                let dst = vaddr.as_mut_ptr::<u8>();
                core::ptr::copy_nonoverlapping(buf.as_ptr(), dst, file_size);
            }

            log::debug!("ELF:   Copied {} bytes to 0x{:x}", file_size, vaddr.as_u64());
        }

        // Switch back to kernel page table
        unsafe { Cr3::write(old_cr3, cr3_flags); }

        // BSS (uninitialized data) is already zeroed since we zero all pages
        if mem_size > file_size {
            let bss_size = mem_size - file_size;
            log::debug!("ELF:   BSS: {} bytes (already zeroed)", bss_size);
        }

        // Record segment for metadata
        segments.push(ElfSegment {
            vaddr,
            size: mem_size,
            flags,
        });
    }

    if !has_loadable {
        log::error!("ELF: No loadable segments found");
        return Err(ElfLoadError::NoLoadableSegments);
    }

    log::info!("ELF: Successfully loaded {} segments", segments.len());

    Ok(ElfBinary {
        entry_point,
        segments,
    })
}

/// Spawn a userspace process from an ELF binary
///
/// This function:
/// 1. Creates a new userspace process with fresh address space
/// 2. Loads the ELF binary into the process's address space
/// 3. Sets up the user stack
/// 4. Creates the initial thread at the ELF entry point
/// 5. Initializes stdin/stdout/stderr file descriptors
///
/// Returns the ProcessId and initial ThreadId on success.
pub fn spawn_elf_process(
    elf_data: &[u8],
    name: &str,
    args: &[&str],
) -> Result<(ProcessId, ThreadId), ElfLoadError> {
    log::info!("Spawning ELF process '{}' with {} args", name, args.len());

    // Create a new userspace process with dedicated page tables
    let process_id = scheduler::spawn_user_process(name)
        .map_err(|_| ElfLoadError::MemoryAllocationFailed)?;

    // Get kernel CR3 now (BEFORE entering with_process_mut) to avoid deadlock
    // when batch mapping functions need to access page tables
    let kernel_cr3 = paging::get_kernel_cr3();

    // Initialize file descriptors (stdin/stdout/stderr → TTY0)
    scheduler::with_process_mut(process_id, |process| {
        use crate::io::TtyDevice;
        use alloc::sync::Arc;

        let tty_stdin = Arc::new(TtyDevice::new(0));
        let tty_stdout = Arc::new(TtyDevice::new(0));
        let tty_stderr = Arc::new(TtyDevice::new(0));

        process.fd_table.insert(0, tty_stdin);   // stdin
        process.fd_table.insert(1, tty_stdout);  // stdout
        process.fd_table.insert(2, tty_stderr);  // stderr

        log::debug!("Initialized FD table: stdin/stdout/stderr → TTY0");
    })
    .ok_or(ElfLoadError::MemoryAllocationFailed)?;

    // Load ELF binary and set up user stack
    // Do this in a scope to release the process lock before creating thread
    let (entry_point, _page_table_root, stack_ptr) = scheduler::with_process_mut(process_id, |process| {
        // Load ELF binary into process address space
        // Pass kernel_cr3 to avoid deadlock when mapping pages
        let binary = load_elf_binary(elf_data, &mut process.address_space, kernel_cr3)?;

        log::info!("ELF process '{}' loaded, entry point: 0x{:x}",
                   name, binary.entry_point.as_u64());

        // Set up user stack (16 MB at top of user space)
        const USER_STACK_SIZE: usize = 16 * 1024 * 1024; // 16 MB
        const USER_STACK_TOP: u64 = 0x8000_0000; // 2 GB
        const USER_STACK_BOTTOM: u64 = USER_STACK_TOP - USER_STACK_SIZE as u64;

        log::debug!("ELF: Setting up user stack ({}KB)", USER_STACK_SIZE / 1024);
        log::debug!("ELF:   Stack range: 0x{:x} - 0x{:x}", USER_STACK_BOTTOM, USER_STACK_TOP);

        // Map user stack pages using batch operation for performance
        let page_count = USER_STACK_SIZE / 4096;
        let page_table_root = process.address_space.page_table_root;

        // Stack mapping flags
        let stack_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE;

        // Allocate all frames and prepare mappings for batch operation
        let mut mappings = alloc::vec::Vec::with_capacity(page_count);
        for page_idx in 0..page_count {
            let page_vaddr = VirtAddr::new(USER_STACK_BOTTOM + (page_idx as u64 * 4096));

            // Allocate physical frame for stack page
            let frame = phys::alloc_frame()
                .ok_or(ElfLoadError::MemoryAllocationFailed)?;
            let phys_addr = PhysAddr::new(frame.start_address());

            // Zero the frame
            unsafe {
                let phys_ptr = phys_addr.as_u64() as *mut u8;
                core::ptr::write_bytes(phys_ptr, 0, 4096);
            }

            // Add to batch
            mappings.push((page_vaddr, phys_addr, stack_flags));
        }

        // Map all pages in a single batch (2 CR3 switches instead of 8192!)
        paging::map_pages_batch_in_table(page_table_root, &mappings, kernel_cr3)
            .map_err(|_| ElfLoadError::MappingFailed)?;

        log::info!("ELF: User stack mapped ({} pages)", page_count);

        // Set up argc/argv on the stack
        // We need to write to physical addresses since userspace virtual addresses
        // aren't accessible from kernel page tables

        // Helper to convert virtual stack address to physical
        let virt_to_phys = |vaddr: u64| -> u64 {
            let offset = vaddr - USER_STACK_BOTTOM;
            let page_idx = (offset / 4096) as usize;
            let page_offset = offset % 4096;
            mappings[page_idx].1.as_u64() + page_offset
        };

        let mut stack_ptr = USER_STACK_TOP;

        // Build full argv: [program_name, ...args]
        let mut full_args = alloc::vec![name];
        full_args.extend_from_slice(args);
        let argc = full_args.len();

        // Reserve space and write argument strings
        let mut arg_pointers = alloc::vec::Vec::new();
        for arg in full_args.iter().rev() {
            let arg_len = arg.len() + 1; // Include null terminator
            stack_ptr -= arg_len as u64;

            // Write string to stack via physical address
            let phys_addr = virt_to_phys(stack_ptr);
            unsafe {
                let dest = phys_addr as *mut u8;
                core::ptr::copy_nonoverlapping(arg.as_ptr(), dest, arg.len());
                core::ptr::write(dest.add(arg.len()), 0); // Null terminator
            }

            arg_pointers.push(stack_ptr);
        }
        arg_pointers.reverse();

        // Align stack to 8 bytes
        stack_ptr &= !7;

        // Push NULL (end of argv)
        stack_ptr -= 8;
        unsafe {
            let phys_addr = virt_to_phys(stack_ptr);
            core::ptr::write(phys_addr as *mut u64, 0);
        }

        // Push argv pointers
        for &arg_ptr in arg_pointers.iter().rev() {
            stack_ptr -= 8;
            unsafe {
                let phys_addr = virt_to_phys(stack_ptr);
                core::ptr::write(phys_addr as *mut u64, arg_ptr);
            }
        }

        // Push argc
        stack_ptr -= 8;
        unsafe {
            let phys_addr = virt_to_phys(stack_ptr);
            core::ptr::write(phys_addr as *mut u64, argc as u64);
        }

        log::info!("ELF: Set up argc={} argv on stack, RSP=0x{:x}", argc, stack_ptr);
        for (i, &ptr) in arg_pointers.iter().enumerate() {
            log::info!("ELF:   argv[{}] = 0x{:x}", i, ptr);
        }

        // Verify the write by reading back argc from physical memory
        unsafe {
            let phys_addr = virt_to_phys(stack_ptr);
            let written_argc = core::ptr::read(phys_addr as *const u64);
            log::info!("ELF: VERIFY: Read back argc={} from phys 0x{:x}", written_argc, phys_addr);

            // CRITICAL: Verify this physical address is mapped to the stack virtual address
            let offset = stack_ptr - USER_STACK_BOTTOM;
            let page_idx = (offset / 4096) as usize;
            let page_offset = offset % 4096;
            log::info!("ELF: Stack virt=0x{:x} -> page_idx={}, page_offset=0x{:x}",
                       stack_ptr, page_idx, page_offset);
            log::info!("ELF: mappings[{}] = virt:0x{:x} -> phys:0x{:x}",
                       page_idx, mappings[page_idx].0.as_u64(), mappings[page_idx].1.as_u64());
        }

        Ok((binary.entry_point, page_table_root, VirtAddr::new(stack_ptr)))
    })
    .ok_or(ElfLoadError::MemoryAllocationFailed)??;

    // Now create the thread AFTER releasing the process lock to avoid deadlock
    log::debug!("ELF: Creating userspace thread");
    let thread_id = scheduler::spawn_thread_in_process(
        userspace_entry_trampoline,
        name,
        process_id,
    );

    // Set up the thread's interrupt context to enter userspace
    // Use the stack pointer that has argc/argv pushed on it
    scheduler::setup_userspace_thread(
        thread_id,
        entry_point,
        stack_ptr,
    ).map_err(|_| ElfLoadError::ThreadSetupFailed)?;

    log::info!("ELF: Thread configured for userspace entry");

    // FIXME: Logging after thread creation causes deadlock
    // log::info!("ELF process '{}' spawned: PID={:?}, TID={:?}",
    //            name, process_id, thread_id);

    Ok((process_id, thread_id))
}
