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
use crate::token::{AddressSpaceId, ObjectRef};
use klibcluu::{boot_elf::ParsedElf, boot_tar};
use x86_64::structures::paging::{PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

/// Userspace init stack top (16 MB stack, grows down from here)
const INIT_STACK_TOP: u64 = 0x7ff00000;

/// Initrd mapping address in userspace (high address, read-only)
/// Maps at 2GB mark, well above normal userspace regions
const INITRD_USER_BASE: u64 = 0x80000000;

/// Userspace boot info page (contains token + initrd metadata)
const BOOT_INFO_ADDR: u64 = 0x7fe00000;

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
    let init_elf = boot_tar::find_file(initrd_slice, "sys/init").ok_or_else(|| {
        klibcluu::error("Failed to find sys/init in initrd");
        Error::InvalidArgument
    })?;

    klibcluu::trace("Found sys/init (");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, " bytes)", init_elf.len() as u64);

    // Parse and validate init ELF
    let init_parsed = ParsedElf::parse(init_elf).map_err(|_| {
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

    let root_scope = crate::token::OpaqueScope::random();
    let root_token_handle = crate::token::create_token(
        root_scope,
        crate::token::Rights::all(),
        crate::token::Issuer::Kernel,
        crate::token::Timestamp::far_future(),
        ObjectRef::Space(AddressSpaceId::new(0)),
    );

    let boot_frame = crate::mm::pmm_simple::alloc_frame().ok_or_else(|| {
        klibcluu::error("Failed to allocate boot info frame");
        Error::OutOfMemory
    })?;

    unsafe {
        crate::elf::map_user_page(
            BOOT_INFO_ADDR,
            boot_frame,
            true,
            false,
            init_space.page_table_root,
        )?;

        let boot_phys = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(boot_frame));
        let boot_info_virt = crate::mm::physmap::phys_to_virt(boot_phys.start_address());
        let boot_info_ptr = boot_info_virt.as_mut_ptr::<BootInfo>();
        core::ptr::write_bytes(
            boot_info_virt.as_mut_ptr::<u8>(),
            0,
            core::mem::size_of::<BootInfo>(),
        );

        let boot_info = &mut *boot_info_ptr;
        boot_info.root_token = root_token_handle.as_usize();
        boot_info.initrd_phys = initrd_phys;
        boot_info.initrd_size = initrd_size;
    }

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

#[repr(C)]
struct BootInfo {
    root_token: usize,
    initrd_phys: u64,
    initrd_size: u64,
}
