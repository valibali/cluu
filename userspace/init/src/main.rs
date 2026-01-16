#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use libcluu::boot::{
    boot_info, ProcessInfo, CONSOLE_FB_BASE, INITRD_USER_BASE, PARAM_FB_BASE, PARAM_FB_HEIGHT,
    PARAM_FB_PITCH, PARAM_FB_SIZE, PARAM_FB_WIDTH, PARAM_INITRD_SIZE, PROCESS_INFO_ADDR,
};
use libcluu::elf::ElfFile;
use libcluu::tar::find_member;
use libcluu::*;

const PROC_STACK_SIZE: usize = 64 * 1024;
const PROC_STACK_BASE: usize = 0x6f000000;
const PROC_STACK_TOP: usize = PROC_STACK_BASE + PROC_STACK_SIZE;
const STACK_FLAGS: usize = 0x03; // read + write
const STACK_STEP: usize = PROC_STACK_SIZE + 0x1000;

// Service-specific token indices (beyond standard stdin/stdout/stderr/stdlog/registry/proc_cap)
const SVC_TOKEN_LISTEN: usize = 6; // recv endpoint for service requests
const SVC_TOKEN_CAP: usize = 7; // capability token (procmgr)
const SVC_TOKEN_IRQ: usize = 8; // irq token (kbd)

struct ServiceSpec {
    name: &'static str,
    path: &'static str,
    priority: usize,
    rights: Option<Rights>,
    kind: ServiceKind,
}

enum ServiceKind {
    Registry,
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
        name: "registry",
        path: "sys/registry",
        priority: 190,
        rights: None,
        kind: ServiceKind::Registry,
    },
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
    let exit_endpoint = create_exit_endpoint(info.root_token)?;
    let registry_full = endpoint_create(info.root_token)?;
    let registry_endpoint =
        token_derive(registry_full, Rights::IPC_RECV.bits() as usize, u64::MAX)?;
    let registry_send = token_derive(registry_full, Rights::IPC_SEND.bits() as usize, u64::MAX)?;
    let kbd_irq_token = token_derive(
        info.root_token,
        Rights::IRQ_HANDLE.bits() as usize,
        u64::MAX,
    )?;

    let root_token = info.root_token;
    for (index, service) in SERVICE_LIST.iter().enumerate() {
        let child_token = if let Some(rights) = service.rights {
            token_derive(root_token, rights.bits() as usize, u64::MAX)?
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
            registry_endpoint,
            registry_send,
            kbd_irq_token,
        )?;
        debug_print(&format!("init: {} ready", service.name))?;

        let _ = child_token;
    }

    debug_print("init: all critical services created; yielding to scheduler")?;
    yield_cpu()?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_service(
    service: &ServiceSpec,
    token: usize,
    initrd: &[u8],
    index: usize,
    token_share: Option<(usize, usize, usize)>,
    registry_endpoint: usize,
    registry_send: usize,
    kbd_irq_token: usize,
) -> Result<()> {
    let service_bytes = find_member(initrd, service.path).ok_or(Error::NotFound)?;
    debug_print(&format!("init: parsed {} entry", service.name))?;

    let elf = ElfFile::parse(service_bytes)?;
    let stack_top = PROC_STACK_TOP - index * STACK_STEP;
    let space_token = space_create(token)?;
    map_segments(space_token, &elf, service_bytes)?;
    map_stack(space_token, stack_top, PROC_STACK_SIZE, STACK_FLAGS)?;

    // Build ProcessInfo for each service
    let mut tokens = [0usize; 16];
    let mut params = [0u64; 8];

    let proc_cap = derive_proc_cap(token)?;
    tokens[TOKEN_REGISTRY] = registry_send;
    tokens[TOKEN_PROC_CAP] = proc_cap;
    fill_default_endpoints(token, &mut tokens)?;

    match service.kind {
        ServiceKind::Registry => {
            tokens[SVC_TOKEN_LISTEN] = registry_endpoint;
            map_process_info(space_token, 0, 0, &tokens, &params)?;
        }
        ServiceKind::Console => {
            tokens[SVC_TOKEN_LISTEN] = create_grantable_listen_endpoint(token)?;
            let info = boot_info();
            params[PARAM_FB_BASE] = CONSOLE_FB_BASE as u64;
            params[PARAM_FB_SIZE] = info.fb_size;
            params[PARAM_FB_WIDTH] = info.fb_width as u64;
            params[PARAM_FB_HEIGHT] = info.fb_height as u64;
            params[PARAM_FB_PITCH] = info.fb_pitch as u64;
            map_process_info(space_token, 0, 0, &tokens, &params)?;
            map_framebuffer(space_token, info.fb_phys, info.fb_size)?;
        }
        ServiceKind::Kbd => {
            tokens[SVC_TOKEN_LISTEN] = create_listen_endpoint(token)?;
            tokens[SVC_TOKEN_IRQ] = kbd_irq_token;
            map_process_info(space_token, 0, 0, &tokens, &params)?;
        }
        ServiceKind::Tty => {
            tokens[SVC_TOKEN_LISTEN] = create_grantable_listen_endpoint(token)?;
            map_process_info(space_token, 0, 0, &tokens, &params)?;
        }
        ServiceKind::Procmgr => {
            if let Some((cap_token, exit_endpoint, initrd_size)) = token_share {
                tokens[SVC_TOKEN_LISTEN] = exit_endpoint;
                tokens[SVC_TOKEN_CAP] = cap_token;
                params[PARAM_INITRD_SIZE] = initrd_size as u64;
                map_process_info(space_token, 0, 0, &tokens, &params)?;
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

fn fill_default_endpoints(token: usize, tokens: &mut [usize; 16]) -> Result<()> {
    if tokens[TOKEN_STDIN] == 0 {
        tokens[TOKEN_STDIN] = endpoint_create(token)?;
    }
    if tokens[TOKEN_STDOUT] == 0 {
        tokens[TOKEN_STDOUT] = endpoint_create(token)?;
    }
    if tokens[TOKEN_STDERR] == 0 {
        tokens[TOKEN_STDERR] = endpoint_create(token)?;
    }
    if tokens[TOKEN_STDLOG] == 0 {
        tokens[TOKEN_STDLOG] = endpoint_create(token)?;
    }
    Ok(())
}

fn derive_proc_cap(token: usize) -> Result<usize> {
    let rights =
        Rights::CREATE | Rights::IPC_SEND | Rights::IPC_RECV | Rights::IPC_CALL | Rights::GRANT;
    token_derive(token, rights.bits() as usize, u64::MAX)
}

fn create_listen_endpoint(token: usize) -> Result<usize> {
    let endpoint = endpoint_create(token)?;
    token_derive(endpoint, Rights::IPC_RECV.bits() as usize, u64::MAX)
}

fn create_grantable_listen_endpoint(token: usize) -> Result<usize> {
    let endpoint = endpoint_create(token)?;
    let rights = Rights::IPC_RECV | Rights::IPC_SEND | Rights::GRANT;
    token_derive(endpoint, rights.bits() as usize, u64::MAX)
}

fn create_exit_endpoint(token: usize) -> Result<usize> {
    let endpoint = endpoint_create(token)?;
    let rights = Rights::IPC_RECV | Rights::IPC_SEND | Rights::GRANT;
    token_derive(endpoint, rights.bits() as usize, u64::MAX)
}

/// Map the unified ProcessInfo structure into a child's address space.
fn map_process_info(
    space_token: usize,
    exit_token: usize,
    exit_cookie: usize,
    tokens: &[usize; 16],
    params: &[u64; 8],
) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);

    let info = ProcessInfo {
        exit_token,
        exit_cookie,
        tokens: *tokens,
        params: *params,
    };

    let mut page = [0u8; PAGE_SIZE];
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &info as *const ProcessInfo as *const u8,
            core::mem::size_of::<ProcessInfo>(),
        )
    };
    let offset = PROCESS_INFO_ADDR - page_base;
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
        PAGE_SIZE,
    )?;
    Ok(())
}

fn map_framebuffer(space_token: usize, fb_phys: u64, fb_size: u64) -> Result<()> {
    const READ_WRITE: usize = 0x03;
    if fb_phys == 0 || fb_size == 0 {
        return Ok(());
    }
    let num_pages = (fb_size as usize).div_ceil(PAGE_SIZE);
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
    let num_pages = initrd_size.div_ceil(PAGE_SIZE);
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
