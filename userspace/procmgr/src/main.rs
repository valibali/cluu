#![no_std]
#![no_main]

extern crate alloc;

use alloc::{collections::BTreeMap, format};
use core::mem::size_of;
use libcluu::boot::{
    process_info, ProcessInfo, PARAM_INITRD_SIZE, PROCESS_INFO_ADDR, TOKEN_PROC_CAP,
    TOKEN_REGISTRY, TOKEN_STDERR, TOKEN_STDIN, TOKEN_STDLOG, TOKEN_STDOUT,
};
use libcluu::elf::ElfFile;
use libcluu::registry;
use libcluu::syscall::thread_destroy;
use libcluu::tar::find_member;
use libcluu::*;

// Service-specific token indices used by init for procmgr
const SVC_TOKEN_LISTEN: usize = 7;
const SVC_TOKEN_CAP: usize = 8;

const SERVICE_STACK_SIZE: usize = 64 * 1024;
const SERVICE_STACK_BASE: usize = 0x6d000000;
const SERVICE_STACK_TOP: usize = SERVICE_STACK_BASE + SERVICE_STACK_SIZE;
const STACK_FLAGS: usize = 0x03; // read + write
                                 // PAGE_SIZE is imported from libcluu::*
const SERVICE_PATH: &str = "bin/shell";
const PROCMGR_EXIT_LABEL: u32 = 1;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if let Err(err) = main_result() {
        let _ = debug_print(&format!("procmgr: fatal error {:?}", err));
        loop {
            let _ = yield_cpu();
        }
    }
    0
}

fn main_result() -> Result<()> {
    let mut manager = ProcessManager::new()?;
    manager.init()?;
    manager.run()
}

struct ProcessManager {
    token: usize,
    exit_endpoint: usize,
    registry_send: usize,
    initrd_size: usize,
    _proc_cap: usize,
    exit_cookie_next: usize,
    exit_table: BTreeMap<usize, usize>,
}

impl ProcessManager {
    fn new() -> Result<Self> {
        let info = process_info();
        Ok(Self {
            token: info.tokens[SVC_TOKEN_CAP],
            exit_endpoint: info.tokens[SVC_TOKEN_LISTEN],
            registry_send: info.tokens[TOKEN_REGISTRY],
            initrd_size: info.params[PARAM_INITRD_SIZE] as usize,
            _proc_cap: info.tokens[TOKEN_PROC_CAP],
            exit_cookie_next: 1,
            exit_table: BTreeMap::new(),
        })
    }

    fn init(&mut self) -> Result<()> {
        registry::init("procmgr")?;
        registry::register_default_outputs()?;
        debug_print("=========================================")?;
        debug_print("  Process Manager Starting")?;
        debug_print("=========================================")?;
        debug_print("Derived procmgr token handle")?;
        debug_print(&format!("  Handle: {}", self.token))?;

        self.spawn_service(SERVICE_PATH, 200)?;
        debug_print("Service spawned; yielding to scheduler")?;
        yield_cpu()?;
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        loop {
            self.poll_exit_notifications()?;
        }
    }

    fn poll_exit_notifications(&mut self) -> Result<()> {
        let registry_endpoint = registry::control_endpoint();
        let tokens = [self.exit_endpoint, registry_endpoint];
        let mut buf = [0u8; 256];
        let (index, len) = match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok(res) => res,
            Err(err) => {
                debug_print(&format!("TRACE: exit recv failed {:?}", err))?;
                return Ok(());
            }
        };
        if index == 1 {
            if let Some((msg, payload)) = parse_message(&buf[..len]) {
                let _ = registry::handle_incoming_message(&msg, payload);
            }
            return Ok(());
        }
        let Some((msg, _payload)) = parse_message(&buf[..len]) else {
            return Ok(());
        };
        if msg.tag.label != PROCMGR_EXIT_LABEL || msg.tag.words < 2 {
            debug_print(&format!(
                "TRACE: exit msg label {} words {}",
                msg.tag.label, msg.tag.words
            ))?;
            return Ok(());
        }

        let cookie = msg.words[0];
        let exit_code = msg.words[1] as i32;
        let thread_token = match self.exit_table.remove(&cookie) {
            Some(token) => token,
            None => return Ok(()),
        };

        debug_print(&format!(
            "procmgr: exit cookie {} (code {})",
            cookie, exit_code
        ))?;
        if thread_destroy(thread_token).is_ok() {
            debug_print(&format!("TRACE: reaped thread token {}", thread_token))?;
        }
        Ok(())
    }

    fn next_exit_cookie(&mut self) -> usize {
        let cookie = self.exit_cookie_next;
        self.exit_cookie_next = self.exit_cookie_next.wrapping_add(1);
        cookie
    }

    fn spawn_service(&mut self, path: &str, priority: usize) -> Result<()> {
        let initrd =
            unsafe { core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, self.initrd_size) };
        let service_bytes = find_member(initrd, path).ok_or(Error::NotFound)?;

        let elf = ElfFile::parse(service_bytes)?;
        debug_print("Parsed service ELF")?;

        let space_token = space_create(self.token)?;
        libcluu::map_segments(space_token, &elf, service_bytes)?;
        libcluu::map_stack(
            space_token,
            SERVICE_STACK_TOP,
            SERVICE_STACK_SIZE,
            STACK_FLAGS,
        )?;

        let send_rights = Rights::IPC_SEND.bits() as usize;
        let child_endpoint = token_derive(self.exit_endpoint, send_rights, u64::MAX)?;
        let cookie = self.next_exit_cookie();
        debug_print(&format!(
            "TRACE: child exit ep {} cookie {}",
            child_endpoint, cookie
        ))?;
        let stdin_endpoint = endpoint_create(self.token)?;
        let stdout_endpoint = endpoint_create(self.token)?;
        let stderr_endpoint = endpoint_create(self.token)?;
        let stdlog_endpoint = endpoint_create(self.token)?;
        let proc_cap = derive_proc_cap(self.token)?;
        let space_grant_token = derive_space_token(space_token)?;
        map_process_info_page(
            space_token,
            child_endpoint,
            cookie,
            stdin_endpoint,
            stdout_endpoint,
            stderr_endpoint,
            stdlog_endpoint,
            self.registry_send,
            proc_cap,
            space_grant_token,
        )?;

        let thread_token = thread_create(
            space_token,
            elf.entry_point as usize,
            SERVICE_STACK_TOP,
            priority,
        )?;

        self.exit_table.insert(cookie, thread_token);
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn map_process_info_page(
    space_token: usize,
    exit_token: usize,
    exit_cookie: usize,
    stdin_token: usize,
    stdout_token: usize,
    stderr_token: usize,
    stdlog_token: usize,
    registry_token: usize,
    proc_cap_token: usize,
    space_grant_token: usize,
) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);

    let mut tokens = [0usize; 16];
    tokens[TOKEN_STDIN] = stdin_token;
    tokens[TOKEN_STDOUT] = stdout_token;
    tokens[TOKEN_STDERR] = stderr_token;
    tokens[TOKEN_STDLOG] = stdlog_token;
    tokens[TOKEN_REGISTRY] = registry_token;
    tokens[TOKEN_PROC_CAP] = proc_cap_token;
    tokens[TOKEN_SPACE] = space_grant_token;

    let info = ProcessInfo {
        exit_token,
        exit_cookie,
        tokens,
        params: [0u64; 8],
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

fn derive_proc_cap(token: usize) -> Result<usize> {
    let rights =
        Rights::CREATE | Rights::IPC_SEND | Rights::IPC_RECV | Rights::IPC_CALL | Rights::GRANT;
    token_derive(token, rights.bits() as usize, u64::MAX)
}

fn derive_space_token(space_token: usize) -> Result<usize> {
    let rights = Rights::SPACE_MAP | Rights::SPACE_GRANT;
    token_derive(space_token, rights.bits() as usize, u64::MAX)
}

fn parse_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    if buf.len() < size_of::<Message>() {
        return None;
    }
    let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
    let mut payload_len = msg.words[0];
    let header = size_of::<Message>();
    if header + payload_len > buf.len() {
        payload_len = 0;
    }
    let end = header + payload_len;
    Some((msg, &buf[header..end]))
}
