//! Kernel Bootstrap
//!
//! This module handles the creation of the initial userspace thread during
//! kernel boot.
//!
//! # L4 Microkernel Design
//!
//! The kernel ONLY creates the init thread. Init is responsible for:
//! - Starting critical processes (procmgr, vfs, ramfs, console)
//! - Managing process lifecycle via syscalls
//!
//! # Bootstrap Flow
//!
//! 1. Load init ELF from initrd
//! 2. Create init's address space
//! 3. Map init ELF segments into address space
//! 4. Map initrd into init's address space (read-only, high address)
//! 5. Create init thread at ELF entry point
//! 6. Add init thread to scheduler (INITMODE/cooperative)
//!
//! This is privileged bootstrap code that runs once during kernel initialization.

use crate::error::Error;
use crate::mm::{self, AddressSpace};
use crate::sched::{Priority, Thread, ThreadFlags, ThreadId, ThreadManager};
use x86_64::{PhysAddr, VirtAddr};

/// Userspace init stack top (16 MB stack, grows down from here)
const INIT_STACK_TOP: u64 = 0x7ff00000;

/// Initrd mapping address in userspace (high address, read-only)
/// Maps at 2GB mark, well above normal userspace regions
const INITRD_USER_BASE: u64 = 0x80000000;

/// Bootstrap the init thread
///
/// This function:
/// 1. Locates init ELF in initrd
/// 2. Creates init's address space and loads ELF
/// 3. Maps initrd into init's address space
/// 4. Creates init thread
/// 5. Adds thread to scheduler
///
/// # Arguments
///
/// * `initrd_phys` - Physical address of initrd
/// * `initrd_size` - Size of initrd in bytes
///
/// # Returns
///
/// ThreadId of the init thread
///
/// # Safety
///
/// - Must be called only once during kernel initialization
/// - Must be called after memory management is initialized
/// - Must be called before starting the scheduler
pub unsafe fn init(initrd_phys: u64, initrd_size: u64) -> Result<ThreadId, Error> {
    klibcluu::info("========================================");
    klibcluu::info("Bootstrap: Creating init thread");
    klibcluu::info("========================================");

    if initrd_phys == 0 || initrd_size == 0 {
        klibcluu::error("No initrd found");
        return Err(Error::InvalidArgument);
    }

    klibcluu::debug("Initrd: phys=0x");
    klibcluu::log_hex(klibcluu::LogLevel::Debug, "", initrd_phys);
    klibcluu::debug(" size=");
    klibcluu::log_dec(klibcluu::LogLevel::Debug, " bytes", initrd_size);

    // Convert physical address to virtual (using physmap)
    let phys_addr = PhysAddr::new(initrd_phys);
    let initrd_virt = mm::physmap::phys_to_virt(phys_addr);
    let initrd_slice = unsafe {
        core::slice::from_raw_parts(initrd_virt.as_u64() as *const u8, initrd_size as usize)
    };

    klibcluu::trace("Parsing initrd (tar archive)...");

    // Find init ELF in tar archive
    let init_elf = tar::find_file(initrd_slice, "sys/init").ok_or_else(|| {
        klibcluu::error("Failed to find sys/init in initrd");
        Error::InvalidArgument
    })?;

    klibcluu::trace("Found sys/init (");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, " bytes)", init_elf.len() as u64);

    // Parse and validate init ELF
    let init_parsed = elf::ParsedElf::parse(init_elf).map_err(|_| {
        klibcluu::error("Failed to parse sys/init ELF");
        Error::InvalidArgument
    })?;

    klibcluu::debug("Init entry point: 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Debug, "", init_parsed.entry_point);

    // Create init's address space
    klibcluu::trace("Creating init address space...");
    let mut init_space = AddressSpace::new_user().map_err(|e| {
        klibcluu::error("Failed to create init address space: ");
        klibcluu::error(e);
        Error::OutOfMemory
    })?;

    // Load init ELF into address space
    klibcluu::trace("Loading init ELF...");
    crate::elf::load_elf(init_elf, &mut init_space).map_err(|_e| {
        klibcluu::error("Failed to load init ELF");
        Error::InvalidOperation
    })?;

    // Allocate and map stack for init thread
    klibcluu::trace("Allocating stack for init thread...");
    const INIT_STACK_SIZE: usize = 64 * 1024; // 64 KB
    crate::mm::allocate_user_stack(&mut init_space, INIT_STACK_TOP, INIT_STACK_SIZE)?;

    // Map initrd into init's address space (read-only)
    klibcluu::trace("Mapping initrd into init's address space...");
    map_initrd_to_userspace(&mut init_space, initrd_phys, initrd_size)?;

    klibcluu::info("");
    klibcluu::info("Creating init thread...");

    // Allocate thread ID

    // Create init thread
    let init_thread = Thread::new(
        ThreadManager::alloc_thread_id(),
        init_space.page_table_root,
        VirtAddr::new(init_parsed.entry_point),
        VirtAddr::new(INIT_STACK_TOP),
        Priority::new(200),       // High priority for init
        ThreadFlags::COOPERATIVE, // Cooperative during INITMODE
    );

    klibcluu::info("  ThreadID: ");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", init_thread.id.as_u64());
    klibcluu::info("  Entry:    0x");
    klibcluu::log_hex(klibcluu::LogLevel::Info, "", init_parsed.entry_point);
    klibcluu::info("  Stack:    0x");
    klibcluu::log_hex(klibcluu::LogLevel::Info, "", INIT_STACK_TOP);

    // Add thread to scheduler
    let thread_id = ThreadManager::add_thread(init_thread);

    // Register init as a critical process (needs to initialize)
    ThreadManager::register_critical_thread(thread_id);

    klibcluu::info("");
    klibcluu::info("Init thread created successfully!");
    klibcluu::info("Scheduler mode: INITMODE (cooperative)");
    klibcluu::info("========================================");

    Ok(thread_id)
}

/// Map initrd into userspace address space (read-only)
///
/// Maps the entire initrd at a high address so init can access
/// the ELF binaries for procmgr and other critical processes.
fn map_initrd_to_userspace(
    space: &mut AddressSpace,
    initrd_phys: u64,
    initrd_size: u64,
) -> Result<(), Error> {
    klibcluu::trace("Mapping initrd into init's address space...");
    klibcluu::trace("  Physical: 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, " size=", initrd_phys);
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", initrd_size);
    klibcluu::trace("  Virtual:  0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", INITRD_USER_BASE);

    // Map initrd as read-only at INITRD_USER_BASE (2GB mark)
    mm::map_phys_to_userspace(
        space,
        INITRD_USER_BASE,
        initrd_phys,
        initrd_size,
        false, // read-only
    )?;

    klibcluu::debug("Initrd mapped at userspace 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Debug, " (read-only)", INITRD_USER_BASE);

    Ok(())
}

/// Minimal ELF64 header parser for bootstrap
///
/// This is a simple parser that extracts just enough information to load
/// an executable. Full ELF parsing is done in userspace by procmgr.
mod elf {
    use crate::error::Error;

    /// ELF magic number
    const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

    /// ELF class: 64-bit
    const ELFCLASS64: u8 = 2;

    /// ELF data encoding: Little-endian
    const ELFDATA2LSB: u8 = 1;

    /// ELF type: Executable file
    const ET_EXEC: u16 = 2;

    /// ELF machine: x86-64
    const EM_X86_64: u16 = 62;

    /// Program header type: Loadable segment
    const PT_LOAD: u32 = 1;

    /// ELF64 File Header (minimum we need)
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct Elf64Ehdr {
        pub e_ident: [u8; 16],
        pub e_type: u16,
        pub e_machine: u16,
        pub e_version: u32,
        pub e_entry: u64,
        pub e_phoff: u64,
        pub e_shoff: u64,
        pub e_flags: u32,
        pub e_ehsize: u16,
        pub e_phentsize: u16,
        pub e_phnum: u16,
        pub e_shentsize: u16,
        pub e_shnum: u16,
        pub e_shstrndx: u16,
    }

    /// ELF64 Program Header
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct Elf64Phdr {
        pub p_type: u32,
        pub p_flags: u32,
        pub p_offset: u64,
        pub p_vaddr: u64,
        pub p_paddr: u64,
        pub p_filesz: u64,
        pub p_memsz: u64,
        pub p_align: u64,
    }

    /// Loadable segment information
    #[derive(Debug, Clone, Copy)]
    pub struct LoadableSegment {
        pub vaddr: u64,
        pub file_offset: u64,
        pub file_size: u64,
        pub mem_size: u64,
        pub flags: u32,
    }

    /// Parsed ELF information
    pub struct ParsedElf {
        pub entry_point: u64,
        pub segments: [Option<LoadableSegment>; 8],
        pub segment_count: usize,
    }

    impl ParsedElf {
        /// Parse ELF file from bytes
        pub fn parse(data: &[u8]) -> Result<Self, Error> {
            // Validate minimum size
            if data.len() < core::mem::size_of::<Elf64Ehdr>() {
                return Err(Error::InvalidArgument);
            }

            // Parse ELF header
            let ehdr = unsafe {
                let ptr = data.as_ptr() as *const Elf64Ehdr;
                &*ptr
            };

            // Validate header
            if ehdr.e_ident[0..4] != ELF_MAGIC {
                return Err(Error::InvalidArgument);
            }
            if ehdr.e_ident[4] != ELFCLASS64 {
                return Err(Error::InvalidArgument);
            }
            if ehdr.e_ident[5] != ELFDATA2LSB {
                return Err(Error::InvalidArgument);
            }
            if ehdr.e_type != ET_EXEC {
                return Err(Error::InvalidArgument);
            }
            if ehdr.e_machine != EM_X86_64 {
                return Err(Error::InvalidArgument);
            }
            if ehdr.e_entry == 0 {
                return Err(Error::InvalidArgument);
            }

            // Parse program headers
            let phoff = ehdr.e_phoff as usize;
            let phnum = ehdr.e_phnum as usize;
            let phentsize = ehdr.e_phentsize as usize;

            if phoff + (phnum * phentsize) > data.len() {
                return Err(Error::InvalidArgument);
            }

            let mut parsed = ParsedElf {
                entry_point: ehdr.e_entry,
                segments: [None; 8],
                segment_count: 0,
            };

            // Extract PT_LOAD segments
            for i in 0..phnum {
                if parsed.segment_count >= 8 {
                    break;
                }

                let ph_offset = phoff + (i * phentsize);
                let phdr = unsafe {
                    let ptr = data.as_ptr().add(ph_offset) as *const Elf64Phdr;
                    &*ptr
                };

                if phdr.p_type == PT_LOAD {
                    parsed.segments[parsed.segment_count] = Some(LoadableSegment {
                        vaddr: phdr.p_vaddr,
                        file_offset: phdr.p_offset,
                        file_size: phdr.p_filesz,
                        mem_size: phdr.p_memsz,
                        flags: phdr.p_flags,
                    });
                    parsed.segment_count += 1;
                }
            }

            if parsed.segment_count == 0 {
                return Err(Error::InvalidArgument);
            }

            Ok(parsed)
        }
    }
}

/// Simple tar archive parser for finding files in initrd
mod tar {
    /// Tar header (POSIX ustar format)
    #[repr(C)]
    struct TarHeader {
        name: [u8; 100],
        mode: [u8; 8],
        uid: [u8; 8],
        gid: [u8; 8],
        size: [u8; 12],
        mtime: [u8; 12],
        checksum: [u8; 8],
        typeflag: u8,
        linkname: [u8; 100],
        magic: [u8; 6],
        version: [u8; 2],
        uname: [u8; 32],
        gname: [u8; 32],
        devmajor: [u8; 8],
        devminor: [u8; 8],
        prefix: [u8; 155],
        _padding: [u8; 12],
    }

    /// Find a file in a tar archive
    ///
    /// Returns a slice containing the file data if found
    pub fn find_file<'a>(archive: &'a [u8], filename: &str) -> Option<&'a [u8]> {
        let mut offset = 0;

        while offset + 512 <= archive.len() {
            let header = unsafe {
                let ptr = archive.as_ptr().add(offset) as *const TarHeader;
                &*ptr
            };

            // Check if this is end of archive (all zeros)
            if header.name[0] == 0 {
                break;
            }

            // Parse filename
            let name_len = header.name.iter().position(|&c| c == 0).unwrap_or(100);
            let name = core::str::from_utf8(&header.name[..name_len]).ok()?;

            // Parse file size (octal string)
            let size_str = core::str::from_utf8(&header.size[..11]).ok()?.trim_end();
            let size = usize::from_str_radix(size_str.trim(), 8).ok()?;

            // Skip header
            offset += 512;

            // Check if this is the file we're looking for
            if name == filename {
                return Some(&archive[offset..offset + size]);
            }

            // Skip to next entry (rounded up to 512 bytes)
            offset += (size + 511) & !511;
        }

        None
    }
}
