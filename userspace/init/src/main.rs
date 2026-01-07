#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use libcluu::boot::{boot_info, ParentInfo, ProcmgrInfo, INITRD_USER_BASE, PARENT_INFO_ADDR, PROCMGR_INFO_ADDR};
use libcluu::elf::ElfFile;
use libcluu::tar::find_member;
use libcluu::*;

const PROC_STACK_SIZE: usize = 64 * 1024;
const PROC_STACK_BASE: usize = 0x6f000000;
const PROC_STACK_TOP: usize = PROC_STACK_BASE + PROC_STACK_SIZE;
const STACK_FLAGS: usize = 0x03; // read + write
const STACK_STEP: usize = PROC_STACK_SIZE + 0x1000;

struct ServiceSpec {
    name: &'static str,
    path: &'static str,
    priority: usize,
    rights: Option<Rights>,
}

const PROCMGR_RIGHTS_BITS: u32 = Rights::READ.bits()
    | Rights::WRITE.bits()
    | Rights::CREATE.bits()
    | Rights::THREAD_CONTROL.bits()
    | Rights::THREAD_SUSPEND.bits()
    | Rights::DESTROY.bits()
    | Rights::SPACE_MAP.bits()
    | Rights::SPACE_UNMAP.bits()
    | Rights::SPACE_GRANT.bits()
    | Rights::IPC_SEND.bits()
    | Rights::IPC_RECV.bits()
    | Rights::IPC_CALL.bits()
    | Rights::IRQ_HANDLE.bits()
    | Rights::IRQ_ACK.bits()
    | Rights::GRANT.bits();

const PROCMGR_RIGHTS: Rights = Rights::from_bits_truncate(PROCMGR_RIGHTS_BITS);

const SERVICE_LIST: &[ServiceSpec] = &[ServiceSpec {
    name: "procmgr",
    path: "sys/procmgr",
    priority: 200,
    rights: Some(PROCMGR_RIGHTS),
}];

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    debug_print("init: bootstrapping critical services")?;

    let info = boot_info();
    let initrd = unsafe {
        core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, info.initrd_size as usize)
    };
    let exit_endpoint = endpoint_create(info.root_token)?;

    let root_token = info.root_token;
    for (index, service) in SERVICE_LIST.iter().enumerate() {
        let child_token = if let Some(rights) = service.rights {
            let derived = token_derive(root_token, rights.bits() as usize, u64::MAX)?;
            derived
        } else {
            root_token
        };
        if service.name == "procmgr" {
            debug_print(&format!("init: procmgr token {}", child_token))?;
        }

        debug_print(&format!("init: launching {}", service.name))?;
        let token_share = if service.name == "procmgr" {
            Some((child_token, exit_endpoint, info.initrd_size as usize))
        } else {
            None
        };
        spawn_service(
            service,
            root_token,
            initrd,
            index,
            token_share,
        )?;
        debug_print(&format!("init: {} ready", service.name))?;

        let _ = child_token;
    }

    debug_print("init: all critical services created; yielding to scheduler")?;
    yield_cpu()?;

    loop {
        yield_cpu()?;
    }
}

fn spawn_service(
    service: &ServiceSpec,
    token: usize,
    initrd: &[u8],
    index: usize,
    token_share: Option<(usize, usize, usize)>,
) -> Result<()> {
    let service_bytes = find_member(initrd, service.path).ok_or(Error::NotFound)?;
    debug_print(&format!("init: parsed {} entry", service.name))?;

    let elf = ElfFile::parse(service_bytes)?;
    let stack_top = PROC_STACK_TOP - index * STACK_STEP;
    let space_token = space_create(token)?;
    map_segments(space_token, &elf, service_bytes)?;
    map_stack(space_token, stack_top, PROC_STACK_SIZE, STACK_FLAGS)?;
    if let Some((shared, exit_endpoint, initrd_size)) = token_share {
        map_procmgr_info(space_token, shared, exit_endpoint, initrd_size)?;
        map_parent_info(space_token)?;
        map_initrd(space_token, initrd, initrd_size)?;
    }

    let thread_token = thread_create(
        space_token,
        elf.entry_point as usize,
        stack_top,
        service.priority,
    )?;
    let _ = thread_token;
    Ok(())
}

fn map_procmgr_info(
    space_token: usize,
    token: usize,
    exit_endpoint: usize,
    initrd_size: usize,
) -> Result<()> {
    const PAGE_SIZE: usize = 4096;
    const READ_ONLY: usize = 0x01;
    let page_base = PROCMGR_INFO_ADDR & !(PAGE_SIZE - 1);

    let mut page = [0u8; PAGE_SIZE];
    let info = ProcmgrInfo {
        token,
        exit_endpoint,
        initrd_size: initrd_size as u64,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &info as *const ProcmgrInfo as *const u8,
            core::mem::size_of::<ProcmgrInfo>(),
        )
    };
    let offset = PROCMGR_INFO_ADDR - page_base;
    let end = offset + bytes.len();
    if end > PAGE_SIZE {
        return Err(Error::InvalidArgument);
    }
    page[offset..end].copy_from_slice(bytes);

    space_map(
        space_token,
        page_base,
        page.as_ptr() as usize,
        READ_ONLY,
        end,
    )?;
    Ok(())
}

fn map_parent_info(space_token: usize) -> Result<()> {
    const PAGE_SIZE: usize = 4096;
    const READ_ONLY: usize = 0x01;
    let page_base = PARENT_INFO_ADDR & !(PAGE_SIZE - 1);

    let mut page = [0u8; PAGE_SIZE];
    let info = ParentInfo {
        exit_endpoint: 0,
        exit_cookie: 0,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &info as *const ParentInfo as *const u8,
            core::mem::size_of::<ParentInfo>(),
        )
    };
    let offset = PARENT_INFO_ADDR - page_base;
    let end = offset + bytes.len();
    if end > PAGE_SIZE {
        return Err(Error::InvalidArgument);
    }
    page[offset..end].copy_from_slice(bytes);

    space_map(
        space_token,
        page_base,
        page.as_ptr() as usize,
        READ_ONLY,
        end,
    )?;
    Ok(())
}

fn map_initrd(space_token: usize, initrd: &[u8], initrd_size: usize) -> Result<()> {
    const PAGE_SIZE: usize = 4096;
    const READ_ONLY: usize = 0x01;
    let mut offset = 0usize;
    while offset < initrd_size {
        let remaining = initrd_size - offset;
        let copy_len = remaining.min(PAGE_SIZE);
        let ptr = initrd[offset..offset + copy_len].as_ptr() as usize;
        space_map(
            space_token,
            INITRD_USER_BASE + offset,
            ptr,
            READ_ONLY,
            copy_len,
        )?;
        offset += PAGE_SIZE;
    }
    Ok(())
}
