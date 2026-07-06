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
const INITRD_USER_BASE: u64 = 0x70000000;

/// Userspace boot info page (contains token + initrd metadata)
const BOOT_INFO_ADDR: u64 = 0x7fe00000;
const BOOT_MANIFEST_HMAC_KEY: [u8; 32] = [
    0x43, 0x4c, 0x55, 0x55, 0x2d, 0x42, 0x4f, 0x4f, 0x54, 0x2d, 0x4d, 0x41, 0x4e, 0x49, 0x46, 0x45,
    0x53, 0x54, 0x2d, 0x4b, 0x45, 0x59, 0x2d, 0x30, 0x31, 0x2d, 0x44, 0x45, 0x56, 0x2d, 0x41, 0x31,
];

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
    verify_boot_manifest(initrd_slice, init_elf)?;

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
    // Phase 2.5: use KERNEL_OWNER — bootstrap loads init before space_repository
    // assigns a real AddressSpaceId. Leaf frames are tagged UserData(KERNEL_OWNER)
    // and intermediate tables PageTable(KERNEL_OWNER). This is acceptable for the
    // primordial init space (only one exists, no cross-space alias is possible).
    crate::elf::load_elf(init_elf, &mut init_space, crate::token::scope::KERNEL_OWNER).map_err(|_e| {
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
    crate::telemetry::record_boot_token_grant();
    klibcluu::info("boot-grant: root token handle=");
    klibcluu::log_dec(
        klibcluu::LogLevel::Info,
        "",
        root_token_handle.as_usize() as u64,
    );

    let clock_token_handle = crate::token::create_token(
        crate::token::OpaqueScope::random(),
        crate::token::Rights::READ,
        crate::token::Issuer::Kernel,
        crate::token::Timestamp::far_future(),
        ObjectRef::Clock,
    );
    crate::telemetry::record_boot_token_grant();
    klibcluu::info("boot-grant: clock token handle=");
    klibcluu::log_dec(
        klibcluu::LogLevel::Info,
        "",
        clock_token_handle.as_usize() as u64,
    );

    let view_mgr_token_handle = crate::token::create_token(
        crate::token::OpaqueScope::random(),
        crate::token::Rights::IPC_SEND
            .union(crate::token::Rights::IPC_RECV)
            .union(crate::token::Rights::IPC_CALL)
            .union(crate::token::Rights::GRANT),
        crate::token::Issuer::Kernel,
        crate::token::Timestamp::far_future(),
        ObjectRef::VfsViewManager { scope_sid: 0, scope_mask: 0xFFFF },
    );
    crate::telemetry::record_boot_token_grant();
    klibcluu::info("boot-grant: view_mgr token handle=");
    klibcluu::log_dec(
        klibcluu::LogLevel::Info,
        "",
        view_mgr_token_handle.as_usize() as u64,
    );

    // BlockRegion cap: DISABLED. VFS-level isolation (VfsViewManager + per-user
    // views) is the isolation mechanism. Block-level isolation is not required
    // at this point — if needed in the future, re-enable this mint and have
    // virtio-blk verify the token at the I/O boundary via verify_block_region.
    // The ObjectRef::BlockRegion variant, TokenDeriveScoped handler, and
    // userspace helpers (token_get_info_block_region, verify_block_region)
    // remain in-tree as dormant infrastructure.
    // let block_region_token_handle = crate::token::create_token(
    //     ...ObjectRef::BlockRegion { device_id: 0, start_sector: 0, sector_count: u64::MAX },
    // );

    let device_region_token_handle = crate::token::create_token(
        crate::token::OpaqueScope::random(),
        crate::token::Rights::device_full(),
        crate::token::Issuer::Kernel,
        crate::token::Timestamp::far_future(),
        ObjectRef::DeviceRegion { device_id: 0, region_kind: 0, base: 0, len: u64::MAX },
    );
    klibcluu::info("boot-grant: device_region token handle=");
    klibcluu::log_dec(
        klibcluu::LogLevel::Info,
        "",
        device_region_token_handle.as_usize() as u64,
    );

    let boot_frame = crate::mm::pmm::alloc_frame().ok_or_else(|| {
        klibcluu::error("Failed to allocate boot info frame");
        Error::OutOfMemory
    })?;

    unsafe {
        // Phase 2.5: bootstrap boot-info frame — use KERNEL_OWNER for same
        // reason as the ELF load above.
        crate::elf::map_user_page(
            BOOT_INFO_ADDR,
            boot_frame,
            true,
            false,
            init_space.page_table_root,
            crate::token::scope::KERNEL_OWNER,
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
        boot_info.clock_token = clock_token_handle.as_usize();
        boot_info.view_mgr_token = view_mgr_token_handle.as_usize();
        boot_info.block_region_token = 0;
        boot_info.device_region_token = device_region_token_handle.as_usize();
        boot_info.initrd_phys = initrd_phys;
        boot_info.initrd_size = initrd_size;
        // BOOTBOOT is #[repr(C, packed)] — use read_unaligned to avoid UB from
        // constructing misaligned references to the static fields.
        boot_info.fb_phys =
            core::ptr::addr_of!(crate::bootboot::bootboot.fb_ptr).read_unaligned() as u64;
        boot_info.fb_size =
            core::ptr::addr_of!(crate::bootboot::bootboot.fb_size).read_unaligned() as u64;
        boot_info.fb_width =
            core::ptr::addr_of!(crate::bootboot::bootboot.fb_width).read_unaligned();
        boot_info.fb_height =
            core::ptr::addr_of!(crate::bootboot::bootboot.fb_height).read_unaligned();
        boot_info.fb_pitch =
            core::ptr::addr_of!(crate::bootboot::bootboot.fb_scanline).read_unaligned();
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
    clock_token: usize,
    view_mgr_token: usize,
    block_region_token: usize,
    device_region_token: usize,
    initrd_phys: u64,
    initrd_size: u64,
    fb_phys: u64,
    fb_size: u64,
    fb_width: u32,
    fb_height: u32,
    fb_pitch: u32,
}

fn verify_boot_manifest(initrd: &[u8], init_elf: &[u8]) -> Result<(), Error> {
    let manifest_bytes = boot_tar::find_file(initrd, "sys/boot.manifest").ok_or_else(|| {
        klibcluu::error("Missing required sys/boot.manifest in initrd");
        Error::InvalidArgument
    })?;

    let manifest = core::str::from_utf8(manifest_bytes).map_err(|_| {
        klibcluu::error("Boot manifest is not valid UTF-8");
        Error::InvalidArgument
    })?;

    let mut version_ok = false;
    let mut init_hash_hex: Option<&str> = None;
    let mut signature_hex: Option<&str> = None;
    let mut canonical = alloc::string::String::new();

    for raw_line in manifest.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(value) = line.strip_prefix("manifest_version=") {
            let version = value.parse::<u32>().map_err(|_| Error::InvalidArgument)?;
            if version != 1 {
                klibcluu::error("Boot manifest version mismatch");
                return Err(Error::InvalidArgument);
            }
            version_ok = true;
            canonical.push_str(line);
            canonical.push('\n');
            continue;
        }

        if let Some(value) = line.strip_prefix("signature=") {
            if signature_hex.is_some() {
                klibcluu::error("Boot manifest has duplicate signature field");
                return Err(Error::InvalidArgument);
            }
            signature_hex = Some(value);
            continue;
        }

        let Some(rest) = line.strip_prefix("service ") else {
            klibcluu::error("Boot manifest has invalid line");
            return Err(Error::InvalidArgument);
        };

        let mut path = None;
        let mut sha256 = None;
        let mut rights = None;

        for token in rest.split_whitespace() {
            let (k, v) = token.split_once('=').ok_or(Error::InvalidArgument)?;
            match k {
                "path" => path = Some(v),
                "sha256" => sha256 = Some(v),
                "rights" => rights = Some(v),
                _ => return Err(Error::InvalidArgument),
            }
        }

        let path = path.ok_or(Error::InvalidArgument)?;
        let sha256 = sha256.ok_or(Error::InvalidArgument)?;
        let _rights = rights.ok_or(Error::InvalidArgument)?;

        if path == "sys/init" {
            if init_hash_hex.is_some() {
                klibcluu::error("Boot manifest contains duplicate sys/init entries");
                return Err(Error::InvalidArgument);
            }
            init_hash_hex = Some(sha256);
        }

        canonical.push_str(line);
        canonical.push('\n');
    }

    if !version_ok {
        klibcluu::error("Boot manifest missing manifest_version=1");
        return Err(Error::InvalidArgument);
    }

    let expected_signature = parse_lower_hex_32(signature_hex.ok_or_else(|| {
        klibcluu::error("Boot manifest missing signature field");
        Error::InvalidArgument
    })?)?;
    let actual_signature =
        klibcluu::crypto::hmac_sha256_fixed(&BOOT_MANIFEST_HMAC_KEY, canonical.as_bytes());
    if actual_signature != expected_signature {
        klibcluu::error("Boot manifest signature verification failed");
        return Err(Error::InvalidArgument);
    }

    let expected_hash = parse_lower_hex_sha256(init_hash_hex.ok_or_else(|| {
        klibcluu::error("Boot manifest missing sys/init entry");
        Error::InvalidArgument
    })?)?;

    let actual_hash = klibcluu::crypto::hash_sha256(init_elf);
    if actual_hash != expected_hash {
        klibcluu::error("Boot manifest hash mismatch for sys/init");
        return Err(Error::InvalidArgument);
    }

    klibcluu::info("Boot manifest verified for sys/init");
    Ok(())
}

fn parse_lower_hex_sha256(s: &str) -> Result<[u8; 32], Error> {
    parse_lower_hex_32(s)
}

fn parse_lower_hex_32(s: &str) -> Result<[u8; 32], Error> {
    if s.len() != 64 {
        return Err(Error::InvalidArgument);
    }

    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for i in 0..32 {
        let hi = hex_nibble(bytes[i * 2]).ok_or(Error::InvalidArgument)?;
        let lo = hex_nibble(bytes[i * 2 + 1]).ok_or(Error::InvalidArgument)?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + (b - b'a')),
        _ => None,
    }
}
