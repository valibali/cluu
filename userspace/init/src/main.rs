#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use libcluu::boot::{boot_info, set_procmgr_token, INITRD_USER_BASE};
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
    priority: 150,
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

    let root_token = info.root_token;
    for (index, service) in SERVICE_LIST.iter().enumerate() {
        let child_token = if let Some(rights) = service.rights {
            let derived = token_derive(root_token, rights.bits() as usize, u64::MAX)?;
            if service.name == "procmgr" {
                set_procmgr_token(derived);
            }
            derived
        } else {
            root_token
        };

        debug_print(&format!("init: launching {}", service.name))?;
        spawn_service(service, root_token, initrd, index)?;
        debug_print(&format!("init: {} ready", service.name))?;

        let _ = child_token;
    }

    debug_print("init: all critical services created; yielding to scheduler")?;
    yield_cpu()?;

    loop {
        yield_cpu()?;
    }
}

fn spawn_service(service: &ServiceSpec, token: usize, initrd: &[u8], index: usize) -> Result<()> {
    let service_bytes = find_member(initrd, service.path).ok_or(Error::NotFound)?;
    debug_print(&format!("init: parsed {} entry", service.name))?;

    let elf = ElfFile::parse(service_bytes)?;
    let stack_top = PROC_STACK_TOP - index * STACK_STEP;
    let space_token = space_create(token)?;
    map_segments(space_token, &elf, service_bytes)?;
    map_stack(space_token, stack_top, PROC_STACK_SIZE, STACK_FLAGS)?;

    thread_create(
        space_token,
        elf.entry_point as usize,
        stack_top,
        service.priority,
    )?;
    Ok(())
}
