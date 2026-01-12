#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use libcluu::boot::{
    boot_info, ConsoleInfo, KbdInfo, ParentInfo, ProcmgrInfo, TtyInfo, CONSOLE_FB_BASE,
    CONSOLE_INFO_ADDR, INITRD_USER_BASE, KBD_INFO_ADDR, PARENT_INFO_ADDR, PROCMGR_INFO_ADDR,
    TTY_INFO_ADDR,
};
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
    kind: ServiceKind,
}

enum ServiceKind {
    Console,
    Kbd,
    Tty,
    Procmgr,
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

const SERVICE_LIST: &[ServiceSpec] = &[
    ServiceSpec {
        name: "procmgr",
        path: "sys/procmgr",
        priority: 200,
        rights: Some(PROCMGR_RIGHTS),
        kind: ServiceKind::Procmgr,
    },
    ServiceSpec {
        name: "kbd",
        path: "sys/kbd",
        priority: 230,
        rights: None,
        kind: ServiceKind::Kbd,
    },
    ServiceSpec {
        name: "tty",
        path: "sys/tty",
        priority: 205,
        rights: None,
        kind: ServiceKind::Tty,
    },
    ServiceSpec {
        name: "console",
        path: "sys/console",
        priority: 210,
        rights: None,
        kind: ServiceKind::Console,
    },
];

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
    let console_endpoint = endpoint_create(info.root_token)?;
    let tty_endpoint = endpoint_create(info.root_token)?;
    let console_send = token_derive(console_endpoint, Rights::IPC_SEND.bits() as usize, u64::MAX)?;
    let tty_send = token_derive(tty_endpoint, Rights::IPC_SEND.bits() as usize, u64::MAX)?;
    let kbd_irq_token = token_derive(
        info.root_token,
        Rights::IRQ_HANDLE.bits() as usize,
        u64::MAX,
    )?;
    let kbd_endpoint = endpoint_create(info.root_token)?;
    let kbd_recv = token_derive(kbd_endpoint, Rights::IPC_RECV.bits() as usize, u64::MAX)?;

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
        let token_share = match service.kind {
            ServiceKind::Procmgr => Some((child_token, exit_endpoint, info.initrd_size as usize)),
            _ => None,
        };
        spawn_service(
            service,
            root_token,
            initrd,
            index,
            token_share,
            console_endpoint,
            console_send,
            tty_endpoint,
            tty_send,
            kbd_irq_token,
            kbd_recv,
        )?;
        debug_print(&format!("init: {} ready", service.name))?;

        let _ = child_token;
    }

    debug_print("init: all critical services created; yielding to scheduler")?;
    yield_cpu()?;

    Ok(())
}

fn spawn_service(
    service: &ServiceSpec,
    token: usize,
    initrd: &[u8],
    index: usize,
    token_share: Option<(usize, usize, usize)>,
    console_endpoint: usize,
    console_send: usize,
    tty_endpoint: usize,
    tty_send: usize,
    kbd_irq_token: usize,
    kbd_recv: usize,
) -> Result<()> {
    let service_bytes = find_member(initrd, service.path).ok_or(Error::NotFound)?;
    debug_print(&format!("init: parsed {} entry", service.name))?;

    let elf = ElfFile::parse(service_bytes)?;
    let stack_top = PROC_STACK_TOP - index * STACK_STEP;
    let space_token = space_create(token)?;
    map_segments(space_token, &elf, service_bytes)?;
    map_stack(space_token, stack_top, PROC_STACK_SIZE, STACK_FLAGS)?;
    match service.kind {
        ServiceKind::Console => {
            map_console_info(space_token, console_endpoint)?;
            map_framebuffer(space_token, boot_info().fb_phys, boot_info().fb_size)?;
        }
        ServiceKind::Kbd => {
            map_kbd_info(space_token, kbd_irq_token, kbd_recv, tty_send)?;
        }
        ServiceKind::Tty => {
            map_tty_info(space_token, tty_endpoint, console_send)?;
        }
        ServiceKind::Procmgr => {
            if let Some((shared, exit_endpoint, initrd_size)) = token_share {
                map_procmgr_bootstrap(
                    space_token,
                    shared,
                    exit_endpoint,
                    initrd_size,
                    tty_endpoint,
                )?;
                map_initrd(space_token, initrd, initrd_size)?;
            }
        }
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

fn map_procmgr_bootstrap(
    space_token: usize,
    token: usize,
    exit_endpoint: usize,
    initrd_size: usize,
    tty_endpoint: usize,
) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let page_base = PROCMGR_INFO_ADDR & !(PAGE_SIZE - 1);

    let mut page = [0u8; PAGE_SIZE];
    let procmgr_info = ProcmgrInfo {
        token,
        exit_endpoint,
        initrd_size: initrd_size as u64,
        tty_endpoint,
    };
    let procmgr_bytes = unsafe {
        core::slice::from_raw_parts(
            &procmgr_info as *const ProcmgrInfo as *const u8,
            core::mem::size_of::<ProcmgrInfo>(),
        )
    };
    let procmgr_offset = PROCMGR_INFO_ADDR - page_base;
    let procmgr_end = procmgr_offset + procmgr_bytes.len();
    if procmgr_end > PAGE_SIZE {
        return Err(Error::InvalidArgument);
    }
    page[procmgr_offset..procmgr_end].copy_from_slice(procmgr_bytes);

    let parent_info = ParentInfo {
        exit_endpoint: 0,
        exit_cookie: 0,
    };
    let parent_bytes = unsafe {
        core::slice::from_raw_parts(
            &parent_info as *const ParentInfo as *const u8,
            core::mem::size_of::<ParentInfo>(),
        )
    };
    let parent_offset = PARENT_INFO_ADDR - page_base;
    let parent_end = parent_offset + parent_bytes.len();
    if parent_end > PAGE_SIZE {
        return Err(Error::InvalidArgument);
    }
    page[parent_offset..parent_end].copy_from_slice(parent_bytes);

    space_map(
        space_token,
        page_base,
        page.as_ptr() as usize,
        READ_ONLY,
        PAGE_SIZE,
    )?;
    Ok(())
}

fn map_console_info(space_token: usize, endpoint: usize) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let page_base = CONSOLE_INFO_ADDR & !(PAGE_SIZE - 1);

    let info = boot_info();
    let mut page = [0u8; PAGE_SIZE];
    let payload = ConsoleInfo {
        fb_base: CONSOLE_FB_BASE as u64,
        fb_size: info.fb_size,
        width: info.fb_width,
        height: info.fb_height,
        pitch: info.fb_pitch,
        endpoint,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &payload as *const ConsoleInfo as *const u8,
            core::mem::size_of::<ConsoleInfo>(),
        )
    };
    let offset = CONSOLE_INFO_ADDR - page_base;
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

fn map_kbd_info(
    space_token: usize,
    irq_token: usize,
    endpoint: usize,
    tty_endpoint: usize,
) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let page_base = KBD_INFO_ADDR & !(PAGE_SIZE - 1);

    let mut page = [0u8; PAGE_SIZE];
    let payload = KbdInfo {
        irq_token,
        endpoint,
        tty_endpoint,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &payload as *const KbdInfo as *const u8,
            core::mem::size_of::<KbdInfo>(),
        )
    };
    let offset = KBD_INFO_ADDR - page_base;
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

fn map_tty_info(space_token: usize, endpoint: usize, console_endpoint: usize) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let page_base = TTY_INFO_ADDR & !(PAGE_SIZE - 1);

    let mut page = [0u8; PAGE_SIZE];
    let payload = TtyInfo {
        endpoint,
        console_endpoint,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &payload as *const TtyInfo as *const u8,
            core::mem::size_of::<TtyInfo>(),
        )
    };
    let offset = TTY_INFO_ADDR - page_base;
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

fn map_framebuffer(space_token: usize, fb_phys: u64, fb_size: u64) -> Result<()> {
    const READ_WRITE: usize = 0x03;
    if fb_phys == 0 || fb_size == 0 {
        return Ok(());
    }
    let num_pages = ((fb_size as usize) + PAGE_SIZE - 1) / PAGE_SIZE;
    space_map_range(
        space_token,
        CONSOLE_FB_BASE,
        fb_phys as usize,
        READ_WRITE | MAP_DEVICE,
        num_pages,
        0, // no data copy for device mapping
    )?;
    Ok(())
}

fn map_initrd(space_token: usize, initrd: &[u8], initrd_size: usize) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let num_pages = (initrd_size + PAGE_SIZE - 1) / PAGE_SIZE;
    space_map_range(
        space_token,
        INITRD_USER_BASE,
        initrd.as_ptr() as usize,
        READ_ONLY,
        num_pages,
        initrd_size,
    )?;
    Ok(())
}
