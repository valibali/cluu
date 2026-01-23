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
use libcluu::ipc::extract_reply_token;
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
const PROCMGR_SPAWN_LABEL: u32 = 2;
const PROCMGR_KILL_LABEL: u32 = 3;
const DEFAULT_PRIORITY: usize = 200;

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
    spawn_endpoint: usize,
    registry_send: usize,
    initrd_size: usize,
    _proc_cap: usize,
    exit_cookie_next: usize,
    pid_next: usize,
    exit_table: BTreeMap<usize, usize>,       // cookie -> thread_token
    exit_notify: BTreeMap<usize, usize>,      // cookie -> notify_endpoint
    pid_to_cookie: BTreeMap<usize, usize>,    // pid -> cookie (for PROC_KILL)
    cookie_to_pid: BTreeMap<usize, usize>,    // cookie -> pid (for exit handling)
    tty_main: usize,
    requested_tty: bool,
}

impl ProcessManager {
    fn new() -> Result<Self> {
        let info = process_info();
        Ok(Self {
            token: info.tokens[SVC_TOKEN_CAP],
            exit_endpoint: info.tokens[SVC_TOKEN_LISTEN],
            spawn_endpoint: 0,
            registry_send: info.tokens[TOKEN_REGISTRY],
            initrd_size: info.params[PARAM_INITRD_SIZE] as usize,
            _proc_cap: info.tokens[TOKEN_PROC_CAP],
            exit_cookie_next: 1,
            pid_next: 2, // PID 1 is typically init
            exit_table: BTreeMap::new(),
            exit_notify: BTreeMap::new(),
            pid_to_cookie: BTreeMap::new(),
            cookie_to_pid: BTreeMap::new(),
            tty_main: 0,
            requested_tty: false,
        })
    }

    fn init(&mut self) -> Result<()> {
        registry::init("procmgr")?;
        registry::register_default_outputs()?;
        self.spawn_endpoint = endpoint_create(self.token)?;
        registry::register_output("spawn", self.spawn_endpoint)?;

        // Wait for tty:main to be available before spawning any processes
        // This ensures children get proper stdout with IPC_CALL rights
        while self.tty_main == 0 {
            match registry::subscribe_output("tty:0", "main") {
                Ok(token) => {
                    self.tty_main = token;
                    let _ = debug_print(&format!("procmgr: tty main granted {}", token));
                    self.requested_tty = true;
                }
                Err(_) => {
                    let _ = yield_cpu();
                }
            }
        }

        debug_print("=========================================")?;
        debug_print("  Process Manager Starting")?;
        debug_print("=========================================")?;
        debug_print("Derived procmgr token handle")?;
        debug_print(&format!("  Handle: {}", self.token))?;

        let _ = self.spawn_service(SERVICE_PATH, DEFAULT_PRIORITY)?;
        debug_print("Service spawned; yielding to scheduler")?;
        yield_cpu()?;
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        loop {
            if self.tty_main == 0 && !self.requested_tty {
                let _ = registry::request_subscription("tty:0", "main");
                self.requested_tty = true;
            }
            self.poll_exit_notifications()?;
        }
    }

    fn poll_exit_notifications(&mut self) -> Result<()> {
        let registry_endpoint = registry::control_endpoint();
        let tokens = [self.exit_endpoint, self.spawn_endpoint, registry_endpoint];
        let mut buf = [0u8; 256];
        let (index, len) = match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok(res) => res,
            Err(err) => {
                let _ = debug_print(&format!("TRACE: exit recv failed {:?}", err));
                return Ok(());
            }
        };
        if index == 2 {
            if let Some((msg, payload)) = parse_message(&buf[..len]) {
                let _ = self.handle_registry_event(&msg, payload);
            }
            return Ok(());
        }
        if index == 1 {
            if let Some((msg, payload)) = parse_message(&buf[..len]) {
                let _ = self.handle_spawn_or_kill_message(&msg, payload);
            }
            return Ok(());
        }
        let Some((msg, _payload)) = parse_message(&buf[..len]) else {
            return Ok(());
        };
        if msg.tag.label != PROCMGR_EXIT_LABEL || msg.tag.words < 2 {
            let _ = debug_print(&format!(
                "TRACE: exit msg label {} words {}",
                msg.tag.label, msg.tag.words
            ));
            return Ok(());
        }

        let cookie = msg.words[0];
        let exit_code = msg.words[1] as i32;
        let thread_token = match self.exit_table.remove(&cookie) {
            Some(token) => token,
            None => return Ok(()),
        };
        
        // Clean up PID tracking
        if let Some(pid) = self.cookie_to_pid.remove(&cookie) {
            self.pid_to_cookie.remove(&pid);
        }
        
        if let Some(notify_endpoint) = self.exit_notify.remove(&cookie) {
            let mut notify_msg = Message::new(PROCMGR_EXIT_LABEL, [0; 6], 2);
            notify_msg.words[0] = cookie;
            notify_msg.words[1] = exit_code as usize;
            let _ = send(notify_endpoint, &notify_msg, IpcFlags::empty());
        }

        let _ = debug_print(&format!(
            "procmgr: exit cookie {} (code {})",
            cookie, exit_code
        ));
        if thread_destroy(thread_token).is_ok() {
            let _ = debug_print(&format!("TRACE: reaped thread token {}", thread_token));
        }
        Ok(())
    }

    fn handle_registry_event(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            if let registry::RegistryEvent::Grant { name, token } = event {
                if name == "main" {
                    self.tty_main = token;
                    let _ = debug_print(&format!("procmgr: tty main granted {}", token));
                }
            } else if let registry::RegistryEvent::SubscribeStatus { code } = event {
                if code != 0 {
                    self.requested_tty = false;
                }
            }
        }
        Ok(())
    }

    fn handle_spawn_or_kill_message(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        // Route to appropriate handler based on label
        if msg.tag.label == PROCMGR_KILL_LABEL {
            return self.handle_kill_message(msg);
        }
        self.handle_spawn_message(msg, payload)
    }

    fn handle_spawn_message(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        let mut reply_msg = Message::new(PROCMGR_SPAWN_LABEL, [0; 6], 2);
        // Extract reply token for call messages (prefer this over legacy reply_endpoint)
        let reply_token = extract_reply_token(msg);
        let reply_endpoint = if msg.tag.words >= 3 { msg.words[2] } else { 0 };
        let notify_endpoint = if msg.tag.words >= 4 { msg.words[3] } else { 0 };
        if msg.tag.label != PROCMGR_SPAWN_LABEL {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            let _ = self.send_spawn_reply(reply_token, reply_endpoint, &reply_msg);
            return Ok(());
        }

        let path = match core::str::from_utf8(payload) {
            Ok(value) => value,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                let _ = self.send_spawn_reply(reply_token, reply_endpoint, &reply_msg);
                return Ok(());
            }
        };
        let priority = if msg.tag.words >= 2 { msg.words[1] } else { DEFAULT_PRIORITY };

        if self.tty_main == 0 {
            if let Ok(token) = registry::subscribe_output("tty:0", "main") {
                self.tty_main = token;
            }
        }

        match self.spawn_service(path, priority) {
            Ok((thread_token, cookie, pid)) => {
                reply_msg.words[0] = 0;
                reply_msg.words[1] = pid; // Return PID instead of thread_token
                reply_msg.words[2] = cookie; // Return cookie for _wait()
                if notify_endpoint != 0 {
                    self.exit_notify.insert(cookie, notify_endpoint);
                }
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }
        if let Err(err) = self.send_spawn_reply(reply_token, reply_endpoint, &reply_msg) {
            let _ = debug_print(&format!("procmgr: spawn reply failed {:?}", err));
        }
        Ok(())
    }

    fn next_exit_cookie(&mut self) -> usize {
        let cookie = self.exit_cookie_next;
        self.exit_cookie_next = self.exit_cookie_next.wrapping_add(1);
        cookie
    }

    fn next_pid(&mut self) -> usize {
        let pid = self.pid_next;
        self.pid_next = self.pid_next.wrapping_add(1);
        pid
    }

    fn spawn_service(&mut self, path: &str, priority: usize) -> Result<(usize, usize, usize)> {
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
        let pid = self.next_pid();
        debug_print(&format!(
            "TRACE: child exit ep {} cookie {} pid {}",
            child_endpoint, cookie, pid
        ))?;
        let stdin_endpoint = endpoint_create(self.token)?;
        let (stdout_endpoint, stderr_endpoint, stdlog_endpoint) = if self.tty_main != 0 {
            // The tty main endpoint already grants IPC_SEND, so reuse it directly.
            (self.tty_main, self.tty_main, self.tty_main)
        } else {
            (
                endpoint_create(self.token)?,
                endpoint_create(self.token)?,
                endpoint_create(self.token)?,
            )
        };
        let proc_cap = derive_proc_cap(self.token)?;
        map_process_info_page(
            space_token,
            child_endpoint,
            cookie,
            pid,
            stdin_endpoint,
            stdout_endpoint,
            stderr_endpoint,
            stdlog_endpoint,
            self.registry_send,
            proc_cap,
            space_token,
        )?;

        let thread_token = thread_create(
            space_token,
            elf.entry_point as usize,
            SERVICE_STACK_TOP,
            priority,
        )?;

        self.exit_table.insert(cookie, thread_token);
        self.pid_to_cookie.insert(pid, cookie);
        self.cookie_to_pid.insert(cookie, pid);
        Ok((thread_token, cookie, pid))
    }
    
    fn handle_kill_message(&mut self, msg: &Message) -> Result<()> {
        let reply_token = extract_reply_token(msg);
        let mut reply_msg = Message::new(PROCMGR_KILL_LABEL, [0; 6], 1);
        
        if msg.tag.words < 2 {
            reply_msg.words[0] = (-1isize) as usize; // EINVAL
            if let Some(token) = reply_token {
                let _ = reply(token, &reply_msg, IpcFlags::empty());
            }
            return Ok(());
        }
        
        let target_pid = msg.words[0];
        let _signal = msg.words[1]; // Signal number (9 = SIGKILL, 15 = SIGTERM)
        
        // Look up the process by PID
        let cookie = match self.pid_to_cookie.get(&target_pid) {
            Some(&c) => c,
            None => {
                reply_msg.words[0] = (-3isize) as usize; // ESRCH - no such process
                if let Some(token) = reply_token {
                    let _ = reply(token, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        };
        
        // Get the thread token
        let thread_token = match self.exit_table.get(&cookie) {
            Some(&t) => t,
            None => {
                reply_msg.words[0] = (-3isize) as usize; // ESRCH
                if let Some(token) = reply_token {
                    let _ = reply(token, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        };
        
        // Destroy the thread
        match thread_destroy(thread_token) {
            Ok(()) => {
                // Clean up tracking
                self.exit_table.remove(&cookie);
                self.pid_to_cookie.remove(&target_pid);
                self.cookie_to_pid.remove(&cookie);
                self.exit_notify.remove(&cookie);
                
                reply_msg.words[0] = 0; // Success
                let _ = debug_print(&format!("procmgr: killed pid {} (cookie {})", target_pid, cookie));
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }
        
        if let Some(token) = reply_token {
            let _ = reply(token, &reply_msg, IpcFlags::empty());
        }
        Ok(())
    }

    fn send_spawn_reply(&self, reply_token: Option<usize>, reply_endpoint: usize, msg: &Message) -> Result<()> {
        // Prefer reply token (from ipc_call), fall back to explicit endpoint, then legacy reply
        if let Some(token) = reply_token {
            reply(token, msg, IpcFlags::empty())
        } else if reply_endpoint != 0 {
            send(reply_endpoint, msg, IpcFlags::empty())
        } else {
            reply(self.spawn_endpoint, msg, IpcFlags::empty())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn map_process_info_page(
    space_token: usize,
    exit_token: usize,
    exit_cookie: usize,
    pid: usize,
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
        pid,
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
