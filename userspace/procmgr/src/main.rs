#![no_std]
#![no_main]

extern crate alloc;

use alloc::{collections::BTreeMap, format};
use libcluu::boot::{
    process_info, ProcessInfo, PROCESS_INFO_ADDR, PARAM_INITRD_SIZE,
    TOKEN_STDIN, TOKEN_STDOUT, TOKEN_STDERR, TOKEN_STDLOG,
};
use libcluu::elf::ElfFile;
use libcluu::syscall::thread_destroy;
use libcluu::tar::find_member;
use libcluu::*;

// Service-specific token indices used by init for procmgr
const SVC_TOKEN_LISTEN: usize = 0;
const SVC_TOKEN_CAP: usize = 1;
const SVC_TOKEN_TTY_SEND: usize = 2;

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
    tty_send: usize,  // send-only token to tty
    initrd_size: usize,
    exit_cookie_next: usize,
    exit_table: BTreeMap<usize, usize>,
}

impl ProcessManager {
    fn new() -> Result<Self> {
        let info = process_info();
        Ok(Self {
            token: info.tokens[SVC_TOKEN_CAP],
            exit_endpoint: info.tokens[SVC_TOKEN_LISTEN],
            tty_send: info.tokens[SVC_TOKEN_TTY_SEND],
            initrd_size: info.params[PARAM_INITRD_SIZE] as usize,
            exit_cookie_next: 1,
            exit_table: BTreeMap::new(),
        })
    }

    fn init(&mut self) -> Result<()> {
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
        let mut msg = Message::new(PROCMGR_EXIT_LABEL, [0; 6], 0);
        if let Err(err) = recv(self.exit_endpoint, &mut msg, IpcFlags::empty()) {
            debug_print(&format!("TRACE: exit recv failed {:?}", err))?;
            return Ok(());
        }

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
        libcluu::map_stack(space_token, SERVICE_STACK_TOP, SERVICE_STACK_SIZE, STACK_FLAGS)?;

        let send_rights = Rights::IPC_SEND.bits() as usize;
        let child_endpoint = token_derive(self.exit_endpoint, send_rights, u64::MAX)?;
        let cookie = self.next_exit_cookie();
        debug_print(&format!(
            "TRACE: child exit ep {} cookie {}",
            child_endpoint, cookie
        ))?;
        let stdin_endpoint = endpoint_create(self.token)?;
        // self.tty_send is already a send token from init, use it directly
        let tty_send = self.tty_send;
        map_process_info_page(
            space_token,
            child_endpoint,
            cookie,
            stdin_endpoint,
            tty_send,
            tty_send,
            tty_send,
        )?;
        register_tty(stdin_endpoint, tty_send)?;

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

fn map_process_info_page(
    space_token: usize,
    exit_token: usize,
    exit_cookie: usize,
    stdin_token: usize,
    stdout_token: usize,
    stderr_token: usize,
    stdlog_token: usize,
) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);

    let mut tokens = [0usize; 16];
    tokens[TOKEN_STDIN] = stdin_token;
    tokens[TOKEN_STDOUT] = stdout_token;
    tokens[TOKEN_STDERR] = stderr_token;
    tokens[TOKEN_STDLOG] = stdlog_token;

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

fn register_tty(stdin_endpoint: usize, tty_endpoint: usize) -> Result<()> {
    let msg = Message::new(
        libcluu::ipc::TTY_REGISTER_LABEL,
        [stdin_endpoint, 0, 0, 0, 0, 0],
        1,
    );
    libcluu::ipc::send(tty_endpoint, &msg, IpcFlags::empty())?;
    Ok(())
}
