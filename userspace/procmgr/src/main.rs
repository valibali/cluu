#![no_std]
#![no_main]

extern crate alloc;

use alloc::{collections::BTreeMap, format, vec::Vec};
use core::mem::size_of;
use libcluu::boot::{
    process_info,
    ProcessInfo,
    PARAM_INITRD_SIZE,
    PROCESS_INFO_ADDR,
    // New token slot constants
    TOKEN_CLOCK,
    TOKEN_EXTRA_0,
    TOKEN_EXTRA_1,
    TOKEN_IPC,
    TOKEN_REGISTRY,
    TOKEN_SELF,
    TOKEN_SPACE,
    TOKEN_STDERR,
    TOKEN_STDIN,
    TOKEN_STDLOG,
    TOKEN_STDOUT,
};
use libcluu::elf::ElfFile;
use libcluu::fs::client::VfsClient;
use libcluu::ipc::extract_reply_token;
use libcluu::registry;
use libcluu::syscall::thread_destroy;
use libcluu::tar::find_member;
use libcluu::*;

const SERVICE_STACK_SIZE: usize = 64 * 1024;
const SERVICE_STACK_BASE: usize = 0x6d000000;
const SERVICE_STACK_TOP: usize = SERVICE_STACK_BASE + SERVICE_STACK_SIZE;
const STACK_FLAGS: usize = 0x03; // read + write
                                 // PAGE_SIZE is imported from libcluu::*
const SERVICE_PATH: &str = "/dev/initrd/bin/shell";
const SHELL_AUTOSTART_CMD: &str = match option_env!("CLUU_SHELL_AUTOSTART_CMD") {
    Some(cmd) => cmd,
    None => "spawn hello",
};
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
    exit_table: BTreeMap<usize, usize>,  // cookie -> thread_token
    exit_notify: BTreeMap<usize, usize>, // cookie -> notify_endpoint
    sender_notify_endpoint: BTreeMap<usize, usize>, // sender_tid -> notify_endpoint
    sender_live_children: BTreeMap<usize, usize>, // sender_tid -> active child count
    pid_to_cookie: BTreeMap<usize, usize>, // pid -> cookie (for PROC_KILL)
    cookie_to_pid: BTreeMap<usize, usize>, // cookie -> pid (for exit handling)
    pid_owner_tid: BTreeMap<usize, usize>, // pid -> authenticated owner thread id
    tty_main: usize,
    requested_tty: bool,
    vfs_endpoint: usize,    // VFS service endpoint
    space_token: usize,     // Our address space token for grants
    grant_base_next: usize, // Reused base address for grant buffer
    clock_token: usize,
}

impl ProcessManager {
    fn new() -> Result<Self> {
        let info = process_info();
        Ok(Self {
            // TOKEN_EXTRA_0: exit notification endpoint from init
            exit_endpoint: info.tokens[TOKEN_EXTRA_0],
            // TOKEN_EXTRA_1: elevated capability token for process management
            token: info.tokens[TOKEN_EXTRA_1],
            spawn_endpoint: 0,
            registry_send: info.tokens[TOKEN_REGISTRY],
            initrd_size: info.params[PARAM_INITRD_SIZE] as usize,
            _proc_cap: info.tokens[TOKEN_IPC], // Now using TOKEN_IPC
            exit_cookie_next: 1,
            pid_next: 2, // PID 1 is typically init
            exit_table: BTreeMap::new(),
            exit_notify: BTreeMap::new(),
            sender_notify_endpoint: BTreeMap::new(),
            sender_live_children: BTreeMap::new(),
            pid_to_cookie: BTreeMap::new(),
            cookie_to_pid: BTreeMap::new(),
            pid_owner_tid: BTreeMap::new(),
            tty_main: 0,
            requested_tty: false,
            vfs_endpoint: 0,
            space_token: info.tokens[TOKEN_SPACE],
            grant_base_next: 0x50100000, // Start after virtqueue region
            clock_token: info.tokens[TOKEN_CLOCK],
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

        let (shell_argv_payload, shell_argc) = build_shell_argv_payload(SHELL_AUTOSTART_CMD);
        let _ = self.spawn_service(
            SERVICE_PATH,
            DEFAULT_PRIORITY,
            &shell_argv_payload,
            shell_argc,
            0, // system-managed bootstrap spawn
        )?;
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
        let (index, len, sender_tid) =
            match libcluu::syscall::ipc_recv_any_with_sender(&tokens, &mut buf, u64::MAX) {
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
                let _ = self.handle_spawn_or_kill_message(&msg, payload, sender_tid);
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
            if let Some(owner_tid) = self.pid_owner_tid.remove(&pid) {
                self.on_child_reaped(owner_tid);
            }
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

    fn handle_spawn_or_kill_message(
        &mut self,
        msg: &Message,
        payload: &[u8],
        sender_tid: usize,
    ) -> Result<()> {
        // Route to appropriate handler based on label
        if msg.tag.label == PROCMGR_KILL_LABEL {
            return self.handle_kill_message(msg, sender_tid);
        }
        self.handle_spawn_message(msg, payload, sender_tid)
    }

    fn handle_spawn_message(&mut self, msg: &Message, payload: &[u8], sender_tid: usize) -> Result<()> {
        let mut reply_msg = Message::new(PROCMGR_SPAWN_LABEL, [0; 6], 2);
        // Spawn must come via ipc_call; do not trust caller-routed reply endpoints.
        let reply_token = extract_reply_token(msg);
        let notify_endpoint = if msg.tag.words >= 4 { msg.words[3] } else { 0 };
        let notify_endpoint = match self.resolve_notify_endpoint(sender_tid, notify_endpoint) {
            Ok(endpoint) => endpoint,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                let _ = self.send_spawn_reply(reply_token, &reply_msg);
                return Ok(());
            }
        };
        if msg.tag.label != PROCMGR_SPAWN_LABEL {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            let _ = self.send_spawn_reply(reply_token, &reply_msg);
            return Ok(());
        }

        let path = match parse_cstr(payload) {
            Some(value) => value,
            None => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                let _ = self.send_spawn_reply(reply_token, &reply_msg);
                return Ok(());
            }
        };

        // Require absolute paths (must start with '/')
        if !path.starts_with('/') {
            let _ = debug_print(&format!("procmgr: rejecting relative path '{}'", path));
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            let _ = self.send_spawn_reply(reply_token, &reply_msg);
            return Ok(());
        }

        let argc = if msg.tag.words >= 2 { msg.words[1] } else { 0 };
        let priority = DEFAULT_PRIORITY;

        // Extract argv data: payload is [path\0, argv[0]\0, argv[1]\0, ...]
        // Skip past the path string (including its NUL terminator)
        let path_nul_end = payload
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(payload.len())
            + 1;
        let argv_data = if argc > 0 && path_nul_end < payload.len() {
            &payload[path_nul_end..]
        } else {
            &[]
        };

        if self.tty_main == 0 {
            if let Ok(token) = registry::subscribe_output("tty:0", "main") {
                self.tty_main = token;
            }
        }

        match self.spawn_service(path, priority, argv_data, argc, sender_tid) {
            Ok((_thread_token, cookie, pid)) => {
                reply_msg.words[0] = 0;
                reply_msg.words[1] = pid; // Return PID instead of thread_token
                reply_msg.words[2] = cookie; // Return cookie for _wait()
                if sender_tid != 0 {
                    let entry = self.sender_live_children.entry(sender_tid).or_insert(0);
                    *entry = entry.saturating_add(1);
                }
                if notify_endpoint != 0 {
                    self.exit_notify.insert(cookie, notify_endpoint);
                }
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }
        if let Err(err) = self.send_spawn_reply(reply_token, &reply_msg) {
            let _ = debug_print(&format!("procmgr: spawn reply failed {:?}", err));
        }
        Ok(())
    }

    fn resolve_notify_endpoint(
        &mut self,
        sender_tid: usize,
        requested_notify_endpoint: usize,
    ) -> Result<usize> {
        if sender_tid == 0 {
            return Err(Error::PermissionDenied);
        }
        if requested_notify_endpoint != 0 {
            self.sender_notify_endpoint
                .insert(sender_tid, requested_notify_endpoint);
            return Ok(requested_notify_endpoint);
        }

        Ok(self
            .sender_notify_endpoint
            .get(&sender_tid)
            .copied()
            .unwrap_or(0))
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

    fn on_child_reaped(&mut self, owner_tid: usize) {
        if owner_tid == 0 {
            return;
        }
        if let Some(active) = self.sender_live_children.get_mut(&owner_tid) {
            *active = active.saturating_sub(1);
            if *active == 0 {
                self.sender_live_children.remove(&owner_tid);
                if self.sender_notify_endpoint.remove(&owner_tid).is_some() {
                    let _ = debug_print(&format!(
                        "procmgr: cleared sender notify binding sender_tid={}",
                        owner_tid
                    ));
                }
            }
        }
    }

    fn spawn_service(
        &mut self,
        path: &str,
        priority: usize,
        argv_payload: &[u8],
        argc: usize,
        owner_tid: usize,
    ) -> Result<(usize, usize, usize)> {
        debug_print("Creating address space...")?;
        let space_token = space_create(self.token)?;
        debug_print(&format!("Address space created: {}", space_token))?;

        let mut entry_point = 0usize;
        let mut mapped = false;

        if !path.starts_with("/dev/initrd/") {
            if let Ok(Some(entry)) = self.map_elf_from_vfs(path, space_token) {
                entry_point = entry;
                mapped = true;
                debug_print(&format!("Mapped ELF from VFS (entry=0x{:x})", entry_point))?;
            }
        }

        if !mapped {
            // Fall back to loading bytes in-process.
            let (elf_data, from_vfs) = self.load_elf(path)?;
            let service_bytes: &[u8] = &elf_data;

            let elf = ElfFile::parse(service_bytes)?;
            entry_point = elf.entry_point as usize;
            debug_print(&format!(
                "Parsed ELF from {} (entry=0x{:x}, size={})",
                if from_vfs { "VFS" } else { "initrd" },
                elf.entry_point,
                service_bytes.len()
            ))?;

            debug_print("Mapping ELF segments...")?;
            libcluu::map_segments(space_token, &elf, service_bytes)?;
            debug_print("ELF segments mapped")?;
        } else {
            debug_print("ELF segments mapped")?;
        }

        debug_print("Mapping stack...")?;
        libcluu::map_stack(
            space_token,
            SERVICE_STACK_TOP,
            SERVICE_STACK_SIZE,
            STACK_FLAGS,
        )?;
        debug_print("Stack mapped")?;

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
        let self_cap = derive_self_cap(self.token)?;
        // Derive space token with SPACE_MAP rights for the child
        let child_space_token = match derive_space_token(space_token) {
            Ok(t) => {
                let _ = debug_print(&format!("procmgr: derived child_space_token={}", t));
                t
            }
            Err(e) => {
                let _ = debug_print(&format!("procmgr: derive_space_token FAILED {:?}", e));
                return Err(e);
            }
        };
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
            self_cap,
            child_space_token, // Now properly derived!
            self.clock_token,
            argv_payload,
            argc,
        )?;

        let thread_token = thread_create(space_token, entry_point, SERVICE_STACK_TOP, priority)?;

        self.exit_table.insert(cookie, thread_token);
        self.pid_to_cookie.insert(pid, cookie);
        self.cookie_to_pid.insert(cookie, pid);
        self.pid_owner_tid.insert(pid, owner_tid);
        Ok((thread_token, cookie, pid))
    }

    /// Load ELF data from VFS or initrd.
    /// Returns (data, from_vfs) where from_vfs indicates the source.
    /// Path must be absolute (start with '/').
    /// Initrd is only accessible via /dev/initrd/ prefix.
    fn load_elf(&mut self, path: &str) -> Result<(Vec<u8>, bool)> {
        const INITRD_PREFIX: &str = "/dev/initrd/";

        // Check if path is for initrd
        if let Some(initrd_path) = path.strip_prefix(INITRD_PREFIX) {
            let initrd = unsafe {
                core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, self.initrd_size)
            };
            if let Some(bytes) = find_member(initrd, initrd_path) {
                return Ok((bytes.to_vec(), false));
            }
            return Err(Error::NotFound);
        }

        // All other paths go through VFS
        if let Some(data) = self.load_from_vfs(path) {
            return Ok((data, true));
        }

        Err(Error::NotFound)
    }

    /// Try to load a file from VFS. Returns None if VFS is not available or file not found.
    /// Uses chunked reading to support files of any size.
    fn load_from_vfs(&mut self, path: &str) -> Option<Vec<u8>> {
        // Match VFS grant buffer size for optimal throughput
        const CHUNK_SIZE: usize = 1024 * 1024;

        // #region agent log
        let _ = debug_print(&format!("procmgr: load_from_vfs path={}", path));
        // #endregion

        // Ensure we have VFS endpoint
        if self.vfs_endpoint == 0 {
            self.vfs_endpoint = registry::subscribe_output("vfs", "main").ok()?;
        }
        // #region agent log
        let _ = debug_print(&format!("procmgr: vfs_endpoint={}", self.vfs_endpoint));
        // #endregion

        let client_id = registry::control_endpoint();
        if client_id == 0 {
            return None;
        }

        let client = VfsClient::new(self.vfs_endpoint, client_id);

        // #region agent log
        let _ = debug_print(&format!("procmgr: opening file via VFS..."));
        // #endregion

        // Open the file
        let file = match client.open(path) {
            Ok(f) => f,
            Err(e) => {
                // #region agent log
                let _ = debug_print(&format!("procmgr: VFS open failed {:?}", e));
                // #endregion
                return None;
            }
        };
        // #region agent log
        let _ = debug_print(&format!(
            "procmgr: file opened fd={} size={}",
            file.fd, file.size
        ));
        // #endregion

        if file.size == 0 {
            let _ = client.close(file);
            return None;
        }

        // Use a full-file read for cacheable sizes, otherwise chunked reads.
        const FILE_CACHE_MAX_SIZE: usize = 8 * 1024 * 1024;
        let use_full_read = file.size <= FILE_CACHE_MAX_SIZE;
        let read_window = if use_full_read { file.size } else { CHUNK_SIZE };

        // Map a grant buffer sized for the chosen read window (reused per call).
        let chunk_pages = read_window.div_ceil(PAGE_SIZE);
        let grant_base = self.grant_base_next;

        // #region agent log
        let _ = debug_print(&format!(
            "procmgr: mapping grant buf at {:#x} pages={}",
            grant_base, chunk_pages
        ));
        // #endregion

        match space_map_range(
            self.space_token,
            grant_base,
            0,    // zero-fill
            0x03, // read + write
            chunk_pages,
            0,
        ) {
            Ok(_) | Err(Error::AlreadyExists) => {}
            Err(_) => {
                // #region agent log
                let _ = debug_print("procmgr: space_map_range FAILED");
                // #endregion
                let _ = client.close(file);
                return None;
            }
        }
        // #region agent log
        let _ = debug_print("procmgr: grant buf mapped OK");
        // #endregion

        // Pre-allocate buffer for the full file
        // #region agent log
        let _ = debug_print(&format!("procmgr: allocating Vec capacity={}", file.size));
        // #endregion
        let mut data = Vec::with_capacity(file.size);
        // #region agent log
        let _ = debug_print("procmgr: Vec allocated OK");
        // #endregion

        // Read file in chunks (optionally priming cache with a full read request).
        let mut offset = 0;
        if use_full_read {
            let _ = debug_print(&format!(
                "procmgr: priming cache with full read_grant size={}",
                file.size
            ));
            let grant = match client.read_grant(file, 0, file.size, self.space_token, grant_base) {
                Ok(grant) => grant,
                Err(e) => {
                    let _ = debug_print(&format!("procmgr: read_grant FAILED {:?}", e));
                    let _ = client.close(file);
                    return None;
                }
            };
            if grant.len == 0 {
                let _ = client.close(file);
                return None;
            }
            let chunk = unsafe {
                let ptr = (grant.base + grant.offset) as *const u8;
                core::slice::from_raw_parts(ptr, grant.len)
            };
            data.extend_from_slice(chunk);
            offset = grant.len;
            let _ = debug_print(&format!(
                "procmgr: primed {} bytes, continue chunked",
                offset
            ));
        }

        // Read file in chunks
        // #region agent log
        let _ = debug_print(&format!(
            "procmgr: starting read loop file_size={}",
            file.size
        ));
        // #endregion
        while offset < file.size {
            let remaining = file.size - offset;
            let read_size = remaining.min(CHUNK_SIZE);

            // #region agent log
            let _ = debug_print(&format!(
                "procmgr: read_grant offset={} size={} grant_base={:#x}",
                offset, read_size, grant_base
            ));
            // #endregion

            match client.read_grant(file, offset, read_size, self.space_token, grant_base) {
                Ok(grant) => {
                    // #region agent log
                    let _ = debug_print(&format!(
                        "procmgr: read_grant OK base={:#x} offset={} len={}",
                        grant.base, grant.offset, grant.len
                    ));
                    // #endregion
                    if grant.len == 0 {
                        break;
                    }
                    // #region agent log
                    let _ = debug_print(&format!(
                        "procmgr: copying from {:#x}",
                        grant.base + grant.offset
                    ));
                    // #endregion
                    let chunk = unsafe {
                        let ptr = (grant.base + grant.offset) as *const u8;
                        core::slice::from_raw_parts(ptr, grant.len)
                    };
                    data.extend_from_slice(chunk);
                    offset += grant.len;
                    // #region agent log
                    let _ = debug_print(&format!("procmgr: chunk copied, total={}", data.len()));
                    // #endregion
                }
                Err(e) => {
                    // #region agent log
                    let _ = debug_print(&format!("procmgr: read_grant FAILED {:?}", e));
                    // #endregion
                    let _ = client.close(file);
                    return None;
                }
            }
        }

        let _ = client.close(file);
        Some(data)
    }

    fn map_elf_from_vfs(&mut self, path: &str, space_token: usize) -> Result<Option<usize>> {
        // Ensure we have VFS endpoint
        if self.vfs_endpoint == 0 {
            match registry::subscribe_output("vfs", "main") {
                Ok(token) => self.vfs_endpoint = token,
                Err(_) => return Ok(None),
            }
        }

        let client_id = registry::control_endpoint();
        if client_id == 0 {
            return Ok(None);
        }

        let client = VfsClient::new(self.vfs_endpoint, client_id);
        let file = match client.open(path) {
            Ok(file) => file,
            Err(_) => return Ok(None),
        };

        let map_token = token_derive(space_token, Rights::SPACE_MAP.bits() as usize, u64::MAX)?;
        let entry = match client.map_elf(file, map_token) {
            Ok(entry) => Some(entry),
            Err(err) => {
                let _ = debug_print(&format!("procmgr: map_elf failed {:?}", err));
                None
            }
        };

        let _ = client.close(file);
        Ok(entry)
    }

    fn handle_kill_message(&mut self, msg: &Message, sender_tid: usize) -> Result<()> {
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

        let owner_tid = match self.pid_owner_tid.get(&target_pid) {
            Some(&owner) => owner,
            None => {
                reply_msg.words[0] = (-3isize) as usize; // ESRCH - unknown ownership/pid
                if let Some(token) = reply_token {
                    let _ = reply(token, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        };
        if sender_tid == 0 || sender_tid != owner_tid {
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            let _ = debug_print(&format!(
                "procmgr: deny kill pid {} sender_tid={} owner_tid={}",
                target_pid, sender_tid, owner_tid
            ));
            if let Some(token) = reply_token {
                let _ = reply(token, &reply_msg, IpcFlags::empty());
            }
            return Ok(());
        }

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
                if let Some(owner_tid) = self.pid_owner_tid.remove(&target_pid) {
                    self.on_child_reaped(owner_tid);
                }
                self.exit_notify.remove(&cookie);

                reply_msg.words[0] = 0; // Success
                let _ = debug_print(&format!(
                    "procmgr: killed pid {} (cookie {})",
                    target_pid, cookie
                ));
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

    fn send_spawn_reply(&self, reply_token: Option<usize>, msg: &Message) -> Result<()> {
        // Require reply token from ipc_call to avoid caller-controlled reply routing.
        if let Some(token) = reply_token {
            reply(token, msg, IpcFlags::empty())
        } else {
            Err(Error::InvalidState)
        }
    }
}

fn build_shell_argv_payload(command: &str) -> (Vec<u8>, usize) {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"shell\0");
    let mut argc = 1usize;

    let trimmed = command.trim();
    if trimmed.is_empty() {
        return (payload, argc);
    }

    for token in trimmed.split_whitespace() {
        payload.extend_from_slice(token.as_bytes());
        payload.push(0);
        argc += 1;
    }

    (payload, argc)
}

fn parse_cstr(payload: &[u8]) -> Option<&str> {
    let end = payload
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(payload.len());
    if end == 0 {
        return None;
    }
    core::str::from_utf8(&payload[..end]).ok()
}

/// Param index for argc (number of command-line arguments).
const PARAM_ARGC: usize = 6;
/// Param index for the byte offset within the info page where argv data starts.
const PARAM_ARGV_OFFSET: usize = 7;

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
    self_cap_token: usize,
    space_grant_token: usize,
    clock_token: usize,
    argv_payload: &[u8],
    argc: usize,
) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);

    let mut tokens = [0usize; 16];
    // Slots 0-3: Standard I/O
    tokens[TOKEN_STDIN] = stdin_token;
    tokens[TOKEN_STDOUT] = stdout_token;
    tokens[TOKEN_STDERR] = stderr_token;
    tokens[TOKEN_STDLOG] = stdlog_token;
    // Slots 4-7: Core capabilities
    tokens[TOKEN_SELF] = self_cap_token;
    tokens[TOKEN_SPACE] = space_grant_token;
    tokens[TOKEN_IPC] = proc_cap_token;
    tokens[TOKEN_CLOCK] = clock_token;
    // Slot 8: System service
    tokens[TOKEN_REGISTRY] = registry_token;
    // Slots 9-15: Contextual (empty for regular programs)

    let mut params = [0u64; 8];
    let info_offset = PROCESS_INFO_ADDR - page_base;
    let info_size = size_of::<ProcessInfo>();
    let argv_data_offset = info_offset + info_size; // byte offset within page

    // Store argc and argv data offset in params
    if argc > 0 && !argv_payload.is_empty() {
        params[PARAM_ARGC] = argc as u64;
        params[PARAM_ARGV_OFFSET] = argv_data_offset as u64;
    }

    let info = ProcessInfo {
        exit_token,
        exit_cookie,
        pid,
        tokens,
        params,
    };

    let mut page = [0u8; PAGE_SIZE];
    let bytes =
        unsafe { core::slice::from_raw_parts(&info as *const ProcessInfo as *const u8, info_size) };
    let end = info_offset + bytes.len();
    if end > PAGE_SIZE {
        return Err(Error::InvalidArgument);
    }
    page[info_offset..end].copy_from_slice(bytes);

    // Write argv data after ProcessInfo (null-terminated strings packed contiguously)
    // The payload format from posix_spawn is: [path\0, arg0\0, arg1\0, ...]
    if argc > 0 && !argv_payload.is_empty() {
        let argv_end = argv_data_offset + argv_payload.len();
        if argv_end <= PAGE_SIZE {
            page[argv_data_offset..argv_end].copy_from_slice(argv_payload);
        }
    }

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

/// Derive a self/thread capability token for child processes.
///
/// TOKEN_SELF provides basic thread control authority (CREATE + GRANT).
/// This is intentionally narrower than TOKEN_IPC to follow least-privilege.
fn derive_self_cap(token: usize) -> Result<usize> {
    let rights = Rights::CREATE | Rights::GRANT;
    token_derive(token, rights.bits() as usize, u64::MAX)
}

/// Derive a space token with SPACE_MAP rights for child processes.
/// This allows the child's allocator to map heap pages and allocate frames.
fn derive_space_token(space_token: usize) -> Result<usize> {
    let rights = Rights::SPACE_MAP | Rights::SPACE_GRANT | Rights::CREATE;
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
