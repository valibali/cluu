#![no_std]
#![no_main]

extern crate alloc;

use alloc::{collections::BTreeMap, collections::BTreeSet, format, string::String, vec::Vec};
use core::mem::{size_of, take};
use libcluu::boot::{
    process_info,
    ProcessInfo,
    CONSOLE_FB_BASE,
    PARAM_CONSOLE_ACTIVE,
    PARAM_CONSOLE_INSTANCE,
    PARAM_FB_BASE,
    PARAM_FB_HEIGHT,
    PARAM_FB_PHYS,
    PARAM_FB_PITCH,
    PARAM_FB_SIZE,
    PARAM_FB_WIDTH,
    PARAM_INITRD_SIZE,
    PARAM_TTY_INSTANCE,
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
use libcluu::cap::CapProfile;
use libcluu::elf::ElfFile;
use libcluu::fs::client::VfsClient;
use libcluu::ipc::extract_reply_id;
use libcluu::ipc::parse_message;
use libcluu::ipc::SharedRing;
use libcluu::ipc::PROCMGR_CONTAINER_LIST_LABEL;
use libcluu::ipc::PROCMGR_CONTAINER_RUN_LABEL;
use libcluu::ipc::PROCMGR_CONTAINER_STATS_LABEL;
use libcluu::ipc::PROCMGR_QUERY_CTTY_LABEL;
use libcluu::ipc::PROCMGR_SPAWN_SERVICE_LABEL;
use libcluu::registry;
use libcluu::syscall::{
    space_destroy, thread_destroy, thread_get_id, thread_resume, thread_set_fault_endpoint,
    thread_suspend, token_revoke,
};
use libcluu::tar::find_member;
use libcluu::*;

/// A list of (src, dst, writable) mount tuples representing a process's VFS view.
type ViewMountList = Vec<(String, String, bool)>;

struct ContainerInstance {
    name: String,
    container_id: u64,
    parent_container_id: u64, // 0 = top-level or detached
    pid: usize,
    image_path: String,
    mapped_pages: u32,
}

struct PendingVfsView {
    client_tid: usize,
    mounts: ViewMountList,
    profile: CapProfile,
    container_id: u64,
}

struct UserRecord {
    home: String,
    shell: String,
    profile: CapProfile,
    escalate: Option<CapProfile>,
    password: String,
}

struct SessionEntry {
    container_id: u64,
    shell_cid: u64,
    pid: usize,
    username: String,
    profile: CapProfile,
    vt_index: usize,
    stdin_endpoint: usize,
}

const SERVICE_STACK_SIZE: usize = 64 * 1024;
const SERVICE_STACK_BASE: usize = 0x6d000000;
const SERVICE_STACK_TOP: usize = SERVICE_STACK_BASE + SERVICE_STACK_SIZE;
const STACK_FLAGS: usize = 0x03; // read + write
                                 // PAGE_SIZE is imported from libcluu::*
const SERVICE_PATH: &str = "/var/images/vt/bin/shell";
const SHELL_AUTOSTART_CMD: &str = match option_env!("CLUU_SHELL_AUTOSTART_CMD") {
    Some(cmd) => cmd,
    None => "",
};
const PROCMGR_EXIT_LABEL: u32 = 1;
const PROCMGR_SPAWN_LABEL: u32 = 2;
const PROCMGR_KILL_LABEL: u32 = 3;
const PROCMGR_FAULT_LABEL: u32 = 0xFA017;
const DEFAULT_PRIORITY: usize = 200;
const SIGINT: usize = 2;
const SIGTERM: usize = 15;
const SIGSTOP: usize = 19;
const SIGCONT: usize = 18;
const SIGKILL: usize = 9;
const MAX_VFS_FILE_CACHE_ENTRIES: usize = 16;
const MAX_NESTING_DEPTH: u32 = 8;

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

/// Maximum number of virtual terminals supported.
const VT_COUNT: usize = 4;

// INITRD_USER_BASE is used for loading ELF binaries from initrd
const INITRD_USER_BASE: usize = libcluu::boot::INITRD_USER_BASE;

struct ProcessManager {
    token: usize,
    exit_endpoint: usize,
    spawn_endpoint: usize,
    fault_endpoint: usize,
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
    pid_to_tid: BTreeMap<usize, usize>,  // pid -> child main thread id
    tid_to_pid: BTreeMap<usize, usize>,  // child main thread id -> pid
    pid_ctty: BTreeMap<usize, u8>,       // pid -> controlling terminal VT index
    pid_to_profile: BTreeMap<usize, CapProfile>, // pid -> capability profile
    pid_to_view: BTreeMap<usize, ViewMountList>, // pid -> VFS view mounts
    cookie_to_space: BTreeMap<usize, usize>, // cookie -> space_token (for space_destroy on exit/kill)
    cookie_to_tokens: BTreeMap<usize, Vec<usize>>, // cookie -> derived tokens/endpoints to revoke on exit
    /// Per-VT tty "main" endpoints.  Index 0 is the boot VT.
    tty_endpoints: [usize; VT_COUNT],
    /// Bitmask: bit N set means subscription for tty:N was requested.
    requested_tty_mask: u8,
    vfs_endpoint: usize,    // VFS service endpoint
    space_token: usize,     // Our address space token for grants
    grant_base_next: usize, // Reused base address for grant buffer
    clock_token: usize,
    spawn_seq_next: usize,
    vfs_file_cache: BTreeMap<String, libcluu::fs::client::VfsFile>,
    pending_vfs_views: Vec<PendingVfsView>,
    manager_vfs_view_registered: bool,
    container_id_next: u64,
    pid_to_container_id: BTreeMap<usize, u64>,
    /// PIDs that own their container (created via next_container_id).
    /// Only owners trigger container cleanup on exit.
    container_owner_pids: BTreeSet<usize>,
    container_instances: BTreeMap<u64, ContainerInstance>,
    container_children: BTreeMap<u64, Vec<u64>>, // parent_cid -> child cids
    autostart_done: bool,
    auto_login_done: bool,
    user_records: BTreeMap<String, UserRecord>,
    session_table: BTreeMap<u64, SessionEntry>,
    vt_to_session: [u64; VT_COUNT],
    /// Framebuffer dimensions cached from /proc/fb; zero until console is spawned.
    fb_width: u32,
    fb_height: u32,
    /// Container ID of the vtmgr service (set during autostart).
    vtmgr_container_id: u64,
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
            fault_endpoint: 0,
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
            pid_to_tid: BTreeMap::new(),
            tid_to_pid: BTreeMap::new(),
            pid_ctty: BTreeMap::new(),
            pid_to_profile: BTreeMap::new(),
            pid_to_view: BTreeMap::new(),
            cookie_to_space: BTreeMap::new(),
            cookie_to_tokens: BTreeMap::new(),
            tty_endpoints: [0; VT_COUNT],
            requested_tty_mask: 0,
            vfs_endpoint: 0,
            space_token: info.tokens[TOKEN_SPACE],
            grant_base_next: 0x50100000, // Start after virtqueue region
            clock_token: info.tokens[TOKEN_CLOCK],
            spawn_seq_next: 1,
            vfs_file_cache: BTreeMap::new(),
            pending_vfs_views: Vec::new(),
            manager_vfs_view_registered: false,
            container_id_next: 1,
            pid_to_container_id: BTreeMap::new(),
            container_owner_pids: BTreeSet::new(),
            container_instances: BTreeMap::new(),
            container_children: BTreeMap::new(),
            autostart_done: false,
            auto_login_done: false,
            user_records: BTreeMap::new(),
            session_table: BTreeMap::new(),
            vt_to_session: [0; VT_COUNT],
            fb_width: 0,
            fb_height: 0,
            vtmgr_container_id: 0,
        })
    }

    fn clock_sample(&self) -> u64 {
        if self.clock_token == 0 {
            return 0;
        }
        clock_now(self.clock_token).unwrap_or(0)
    }

    fn next_spawn_seq(&mut self) -> usize {
        let seq = self.spawn_seq_next;
        self.spawn_seq_next = self.spawn_seq_next.wrapping_add(1);
        seq
    }

    fn next_container_id(&mut self) -> u64 {
        let id = self.container_id_next;
        debug_assert!(id > 0, "container_id counter wrapped to 0");
        self.container_id_next = self.container_id_next.wrapping_add(1);
        id
    }

    /// Walk the container parent chain to find the session's VT index.
    fn resolve_caller_vt(&self, sender_tid: usize) -> usize {
        let caller_pid = self.tid_to_pid.get(&sender_tid).copied().unwrap_or(0);
        let mut cid = self.pid_to_container_id.get(&caller_pid).copied().unwrap_or(0);
        while cid != 0 {
            if let Some(session) = self.session_table.get(&cid) {
                return session.vt_index;
            }
            if let Some(inst) = self.container_instances.get(&cid) {
                cid = inst.parent_container_id;
            } else {
                break;
            }
        }
        0 // default VT0
    }

    /// Walk sender_tid → pid → container_id → session_table to find the session.
    /// Traverses the container parent chain to find the enclosing session.
    fn resolve_caller_session(&self, sender_tid: usize) -> Option<&SessionEntry> {
        let caller_pid = self.tid_to_pid.get(&sender_tid).copied()?;
        let mut cid = self.pid_to_container_id.get(&caller_pid).copied().unwrap_or(0);
        while cid != 0 {
            if let Some(session) = self.session_table.get(&cid) {
                return Some(session);
            }
            match self.container_instances.get(&cid) {
                Some(inst) => cid = inst.parent_container_id,
                None => break,
            }
        }
        None
    }

    /// Reattach an existing session to a (re)started VT by sending TTY_REGISTER
    /// with the session's shell stdin endpoint. This causes the new tty to
    /// transition from Login mode directly to Terminal mode.
    fn reattach_session_to_vt(&self, vt_index: usize, session_cid: u64) {
        let tty_ep = self.tty_endpoints[vt_index];
        if tty_ep == 0 { return; }
        let stdin_ep = match self.session_table.get(&session_cid) {
            Some(s) => s.stdin_endpoint,
            None => return,
        };
        if stdin_ep == 0 { return; }
        let reg_msg = Message::new(
            libcluu::ipc::TTY_REGISTER_LABEL,
            [stdin_ep, 0, 0, 0, 0, 0],
            1,
        );
        let _ = ipc::send(tty_ep, &reg_msg, IpcFlags::empty());
        let _ = debug_print(&format!(
            "procmgr: reattach session cid={} stdin={} to tty:{}",
            session_cid, stdin_ep, vt_index
        ));
    }

    /// Build a VFS view from a capability profile and user home directory.
    /// Picks profile-based default mounts, then replaces /home/* with the user's home.
    fn build_view_for_profile_and_home(&self, profile: CapProfile, home: &str) -> ViewMountList {
        let base_mounts = if profile.contains(CapProfile::ADMIN) {
            libcluu::vfs_view::admin_session_mounts()
        } else {
            libcluu::vfs_view::default_mounts_for_profile(profile)
        };
        let mut mounts: ViewMountList = base_mounts.iter()
            .filter(|&&(_, dst, _)| !dst.starts_with("/home/"))
            .map(|&(src, dst, w)| (String::from(src), String::from(dst), w))
            .collect();
        mounts.push((String::from(home), String::from(home), true));
        mounts
    }

    /// Compute nesting depth of a container (0 for top-level/detached).
    fn container_depth(&self, container_id: u64) -> u32 {
        let mut depth = 0u32;
        let mut cid = container_id;
        while cid != 0 {
            depth += 1;
            if let Some(inst) = self.container_instances.values().find(|c| c.container_id == cid) {
                cid = inst.parent_container_id;
            } else {
                break;
            }
        }
        depth
    }

    /// Recursively destroy all child containers of `parent_cid`.
    /// Kills each child's entrypoint process and cleans up container state.
    fn destroy_container_children(&mut self, parent_cid: u64) {
        let children = match self.container_children.remove(&parent_cid) {
            Some(c) => c,
            None => return,
        };
        for child_cid in children {
            if let Some(inst) = self.container_instances.remove(&child_cid) {
                let child_pid = inst.pid;
                let _ = debug_print(&format!(
                    "procmgr: cascading kill container cid={} pid={} (parent={})",
                    child_cid, child_pid, parent_cid
                ));
                // Kill the entrypoint process
                if let Some(&cookie) = self.pid_to_cookie.get(&child_pid) {
                    if let Some(thread_token) = self.exit_table.remove(&cookie) {
                        let _ = thread_destroy(thread_token);
                    }
                    let child_tid = self.pid_to_tid.get(&child_pid).copied().unwrap_or(0);
                    self.clear_vfs_view_for_tid(child_tid);
                    self.pid_to_cookie.remove(&child_pid);
                    self.pid_to_container_id.remove(&child_pid);
                    self.cookie_to_pid.remove(&cookie);
                    self.clear_pid_runtime_state(child_pid);
                    if let Some(owner_tid) = self.pid_owner_tid.remove(&child_pid) {
                        self.on_child_reaped(owner_tid);
                    }
                    if let Some(notify_ep) = self.exit_notify.remove(&cookie) {
                        let mut notify_msg = Message::new(PROCMGR_EXIT_LABEL, [0; 6], 2);
                        notify_msg.words[0] = cookie;
                        notify_msg.words[1] = 128 + SIGKILL;
                        let _ = send(notify_ep, &notify_msg, IpcFlags::empty());
                    }
                    if let Some(st) = self.cookie_to_space.remove(&cookie) {
                        let _ = space_destroy(st);
                    }
                    if let Some(tokens) = self.cookie_to_tokens.remove(&cookie) {
                        for tok in tokens {
                            let _ = token_revoke(tok);
                        }
                    }
                }
                // Clean up container storage
                if child_cid > 0
                    && !self.pid_to_container_id.values().any(|&cid| cid == child_cid)
                {
                    let _ = send_vfs_container_cleanup(self.vfs_endpoint, child_cid, 1);
                }
                // Recursively destroy grandchildren
                self.destroy_container_children(child_cid);
            }
        }
    }

    /// Create per-container directories via VFS before registering the container view.
    /// /var and /var/containers are pre-created on the ext2 image at build time.
    /// If the container image has a `data/` directory, its files are hardlinked
    /// into the container's `/data/` directory (zero-copy seeding).
    /// Returns true on success, false on failure (caller should degrade gracefully).
    fn create_container_dirs(&mut self, container_id: u64, image_name: &str) -> bool {
        if self.vfs_endpoint == 0 {
            if self.ensure_vfs_endpoint().is_err() {
                return false;
            }
        }
        let client = VfsClient::new(self.vfs_endpoint, 0);
        let base = format!("/var/containers/c-{}", container_id);
        for dir in [
            base.as_str(),
            &format!("{}/data", base),
            &format!("{}/tmp", base),
            &format!("{}/log", base),
        ] {
            match client.mkdir(dir, 0o755) {
                Ok(()) | Err(Error::AlreadyExists) => {}
                Err(err) => {
                    let _ = debug_print(&format!(
                        "procmgr: container mkdir failed dir='{}' err={:?}",
                        dir, err
                    ));
                    return false;
                }
            }
        }
        // Seed /data via hardlinks from the image's data/ directory.
        if !image_name.is_empty() {
            let image_data = format!("/var/images/{}/data", image_name);
            if let Ok(entries) = client.readdir(&image_data) {
                let container_data = format!("{}/data", base);
                for entry in &entries {
                    if entry.is_dir {
                        continue; // only seed regular files
                    }
                    let src = format!("{}/{}", image_data, entry.name);
                    let dst = format!("{}/{}", container_data, entry.name);
                    if let Err(e) = client.link(&src, &dst) {
                        let _ = debug_print(&format!(
                            "procmgr: seed link '{}' → '{}' failed: {:?}",
                            src, dst, e
                        ));
                    }
                }
            }
        }
        true
    }

    fn log_spawn_stage(&self, seq: usize, stage: &str, start_ts: u64) {
        let now = self.clock_sample();
        let delta = now.saturating_sub(start_ts);
        let _ = debug_print(&format!(
            "procmgr: spawn_trace seq={} stage={} ts={} dt={}",
            seq, stage, now, delta
        ));
    }

    fn queue_pending_vfs_view(
        &mut self,
        client_tid: usize,
        mounts: ViewMountList,
        profile: CapProfile,
        container_id: u64,
    ) {
        if let Some(pending) = self
            .pending_vfs_views
            .iter_mut()
            .find(|entry| entry.client_tid == client_tid)
        {
            pending.mounts = mounts;
            pending.profile = profile;
            pending.container_id = container_id;
            return;
        }
        self.pending_vfs_views.push(PendingVfsView {
            client_tid,
            mounts,
            profile,
            container_id,
        });
    }

    fn register_manager_vfs_view(&mut self) {
        if self.vfs_endpoint == 0 || self.manager_vfs_view_registered {
            return;
        }
        let manager_mounts = default_view_for_profile(CapProfile::SUPERVISOR);
        match send_vfs_set_view(
            self.vfs_endpoint,
            0,
            &manager_mounts,
            CapProfile::SUPERVISOR,
            0,
        ) {
            Ok(()) => {
                self.manager_vfs_view_registered = true;
            }
            Err(err) => {
                let _ = debug_print(&format!(
                    "procmgr: failed to register manager VFS view err={:?}",
                    err
                ));
            }
        }
    }

    fn flush_pending_vfs_views(&mut self) {
        if self.vfs_endpoint == 0 || self.pending_vfs_views.is_empty() {
            return;
        }
        let pending = take(&mut self.pending_vfs_views);
        for entry in pending {
            if let Err(err) = send_vfs_set_view(
                self.vfs_endpoint,
                entry.client_tid,
                &entry.mounts,
                entry.profile,
                entry.container_id,
            ) {
                let _ = debug_print(&format!(
                    "procmgr: deferred VFS_SET_VIEW failed tid={} err={:?}",
                    entry.client_tid, err
                ));
                self.queue_pending_vfs_view(entry.client_tid, entry.mounts, entry.profile, entry.container_id);
            }
        }
    }

    fn ensure_vfs_endpoint(&mut self) -> Result<()> {
        if self.vfs_endpoint == 0 {
            self.vfs_endpoint = registry::subscribe_output("vfs", "main")?;
            let _ = debug_print(&format!("procmgr: vfs_endpoint={}", self.vfs_endpoint));
        }
        self.register_manager_vfs_view();
        self.flush_pending_vfs_views();
        Ok(())
    }

    fn register_vfs_view_for_thread(
        &mut self,
        thread_token: usize,
        mounts: &[(String, String, bool)],
        profile: CapProfile,
        container_id: u64,
    ) {
        let thread_tid = match thread_get_id(thread_token) {
            Ok(tid) => tid,
            Err(err) => {
                let _ = debug_print(&format!(
                    "procmgr: thread_get_id failed token={} err={:?}",
                    thread_token, err
                ));
                return;
            }
        };

        if self.vfs_endpoint == 0 {
            self.queue_pending_vfs_view(thread_tid, mounts.to_vec(), profile, container_id);
            let _ = self.ensure_vfs_endpoint();
            return;
        }
        if let Err(err) = send_vfs_set_view(self.vfs_endpoint, thread_tid, mounts, profile, container_id) {
            let _ = debug_print(&format!(
                "procmgr: VFS_SET_VIEW failed tid={} err={:?}",
                thread_tid, err
            ));
            self.queue_pending_vfs_view(thread_tid, mounts.to_vec(), profile, container_id);
        }
    }

    fn clear_vfs_view_for_tid(&mut self, client_tid: usize) {
        if client_tid == 0 {
            return;
        }
        if self.vfs_endpoint == 0 {
            self.queue_pending_vfs_view(client_tid, Vec::new(), CapProfile::empty(), 0);
            let _ = self.ensure_vfs_endpoint();
            if self.vfs_endpoint == 0 {
                return;
            }
        }
        let empty_mounts: ViewMountList = Vec::new();
        if let Err(err) = send_vfs_set_view(
            self.vfs_endpoint,
            client_tid,
            &empty_mounts,
            CapProfile::empty(),
            0,
        ) {
            let _ = debug_print(&format!(
                "procmgr: clear VFS_SET_VIEW failed tid={} err={:?}",
                client_tid, err
            ));
            self.queue_pending_vfs_view(client_tid, Vec::new(), CapProfile::empty(), 0);
        }
    }

    fn init(&mut self) -> Result<()> {
        registry::init("procmgr")?;
        registry::register_default_outputs()?;
        self.spawn_endpoint = endpoint_create(self.token)?;
        registry::register_output("spawn", self.spawn_endpoint)?;
        self.fault_endpoint = endpoint_create(self.token)?;

        // Request tty:N main for all VTs (non-blocking); grants arrive via registry events.
        for i in 0..VT_COUNT {
            let name = format!("tty:{}", i);
            if registry::request_subscription(&name, "main").is_ok() {
                self.requested_tty_mask |= 1u8 << i;
            }
        }
        debug_print("procmgr: requested tty:0..3/main subscriptions")?;

        debug_print("=========================================")?;
        debug_print("  Process Manager Starting")?;
        debug_print("=========================================")?;
        debug_print("Derived procmgr token handle")?;
        debug_print(&format!("  Handle: {}", self.token))?;

        // Request VFS "mounted" event (non-blocking); autostart triggers when grant arrives
        let _ = registry::request_subscription("vfs", "mounted");
        debug_print("procmgr: requested vfs/mounted subscription")?;

        yield_cpu()?;
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        let _ = debug_print(&format!(
            "procmgr: entering run loop (exit_ep={} spawn_ep={})",
            self.exit_endpoint, self.spawn_endpoint
        ));
        loop {
            self.poll_exit_notifications()?;
        }
    }

    fn run_autostart(&mut self) {
        let data = match self.read_file_from_vfs("/etc/autostart.toml") {
            Some(d) => d,
            None => {
                let _ = debug_print("procmgr: autostart.toml not found, skipping");
                return;
            }
        };

        let text = match core::str::from_utf8(&data) {
            Ok(s) => s,
            Err(_) => {
                let _ = debug_print("procmgr: autostart.toml is not valid UTF-8");
                return;
            }
        };
        let doc = match libcluu::toml::parse(text) {
            Ok(d) => d,
            Err(e) => {
                let _ = debug_print(&format!("procmgr: autostart.toml parse error: {}", e));
                return;
            }
        };

        let services = doc.array_tables("service");
        let _ = debug_print(&format!("procmgr: autostart {} service(s)", services.len()));

        for svc in &services {
            let image_name = match svc.get_str("image") {
                Some(n) => n,
                None => {
                    let _ = debug_print("procmgr: autostart entry missing 'image' key");
                    continue;
                }
            };
            let _ = debug_print(&format!("procmgr: autostart '{}'", image_name));
            if let Err(e) = self.autostart_container(image_name, svc) {
                let _ = debug_print(&format!(
                    "procmgr: autostart '{}' failed: {:?}", image_name, e
                ));
            }
        }
        let _ = debug_print("procmgr: autostart complete");
    }

    fn parse_users_toml(&mut self) {
        let data = match self.read_file_from_vfs("/etc/users.toml") {
            Some(d) => d,
            None => {
                let _ = debug_print("procmgr: users.toml not found, skipping");
                return;
            }
        };

        let text = match core::str::from_utf8(&data) {
            Ok(s) => s,
            Err(_) => {
                let _ = debug_print("procmgr: users.toml is not valid UTF-8");
                return;
            }
        };
        let doc = match libcluu::toml::parse(text) {
            Ok(d) => d,
            Err(e) => {
                let _ = debug_print(&format!("procmgr: users.toml parse error: {}", e));
                return;
            }
        };

        for table in &doc.tables {
            let username = match table.name.strip_prefix("user.") {
                Some(u) => u,
                None => continue,
            };
            let home = match table.get_str("home") {
                Some(v) => String::from(v),
                None => continue,
            };
            let shell = match table.get_str("shell") {
                Some(v) => String::from(v),
                None => continue,
            };
            let profile = match table.get_str("profile").and_then(parse_profile_str) {
                Some(p) => p,
                None => continue,
            };
            let escalate = table.get_str("escalate").and_then(parse_profile_str);
            let password = table.get_str("password").map(String::from).unwrap_or_default();
            self.user_records.insert(String::from(username), UserRecord {
                home,
                shell,
                profile,
                escalate,
                password,
            });
        }

        let _ = debug_print(&format!(
            "procmgr: loaded {} user record(s)", self.user_records.len()
        ));
    }

    fn try_auto_login(&mut self) {
        if self.auto_login_done { return; }
        if self.user_records.is_empty() { return; }
        if self.tty_endpoints[0] == 0 { return; }

        self.auto_login_done = true;
        let _ = debug_print("procmgr: auto-login root on VT:0");

        let (profile, view_mounts) = match self.user_records.get("root") {
            Some(r) => {
                let p = r.profile;
                let v = self.build_session_view(r);
                (p, v)
            }
            None => return,
        };

        let spawn_seq = self.next_spawn_seq();
        let spawn_start = self.clock_sample();
        let (shell_argv_payload, shell_argc) = build_shell_argv_payload(SHELL_AUTOSTART_CMD);
        let (user_env, user_envc) = build_user_env_payload("root", "/root");

        match self.spawn_service_with_env(SERVICE_PATH, DEFAULT_PRIORITY, &shell_argv_payload, shell_argc, &user_env, user_envc, 1, spawn_seq, spawn_start, &[], profile, 0, 0, &[], None) {
            Ok((thread_token, _cookie, pid, stdin_send)) => {
                let session_cid = self.next_container_id();
                let shell_cid = self.next_container_id();
                self.pid_to_container_id.insert(pid, shell_cid);
                self.container_owner_pids.insert(pid);
                self.register_vfs_view_for_thread(thread_token, &view_mounts, profile, shell_cid);
                self.pid_to_view.insert(pid, view_mounts);
                self.container_instances.insert(shell_cid, ContainerInstance {
                    name: String::from("shell"),
                    container_id: shell_cid,
                    parent_container_id: session_cid,
                    pid,
                    image_path: String::from(SERVICE_PATH),
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                });
                self.container_children.entry(session_cid)
                    .or_insert_with(Vec::new).push(shell_cid);
                self.session_table.insert(session_cid, SessionEntry {
                    container_id: session_cid,
                    shell_cid,
                    pid,
                    username: String::from("root"),
                    profile, vt_index: 0,
                    stdin_endpoint: stdin_send,
                });
                self.vt_to_session[0] = session_cid;
                // Wire shell stdin to tty:0 via TTY_REGISTER so tty transitions to Terminal.
                let tty_ep = self.tty_endpoints[0];
                if tty_ep != 0 && stdin_send != 0 {
                    let reg_msg = Message::new(
                        libcluu::ipc::TTY_REGISTER_LABEL,
                        [stdin_send, 0, 0, 0, 0, 0],
                        1,
                    );
                    let _ = ipc::send(tty_ep, &reg_msg, IpcFlags::empty());
                }
                let _ = debug_print(&format!(
                    "procmgr: auto-login session_cid={} shell_cid={} pid={}",
                    session_cid, shell_cid, pid
                ));
            }
            Err(e) => {
                let _ = debug_print(&format!("procmgr: auto-login failed: {:?}", e));
            }
        }
    }

    fn autostart_container(&mut self, image_name: &str, _svc: &libcluu::toml::TomlTable) -> Result<()> {
        // Read manifest
        let manifest_path = format!("/var/images/{}/manifest.toml", image_name);
        let manifest_contents = self.read_file_from_vfs(&manifest_path)
            .ok_or(Error::NotFound)?;
        let manifest_str = core::str::from_utf8(&manifest_contents)
            .map_err(|_| Error::InvalidArgument)?;
        let doc = libcluu::toml::parse(manifest_str)
            .map_err(|_| Error::InvalidArgument)?;

        // Extract binary path
        let binary = doc.table("exec").and_then(|t| t.get_str("binary"))
            .ok_or(Error::InvalidArgument)?;

        // Build capability profile
        let mut requested_profile = CapProfile::USER;
        if let Some(profile_table) = doc.table("profile") {
            if let Some(caps) = profile_table.get_array("capabilities") {
                for cap_name in caps {
                    if let Some(cap) = parse_capability(cap_name) {
                        requested_profile |= cap;
                    }
                }
            }
        }

        // Container dirs — only create ext2 dirs if persistent storage is needed
        let mut container_id = self.next_container_id();
        let has_persistent_storage = doc
            .table("storage")
            .and_then(|t| t.get_array("persistent_dirs"))
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        let image_dirs: Vec<String> = doc
            .table("storage")
            .and_then(|t| t.get_array("image_dirs"))
            .map(|a| a.iter().map(|s| s.clone()).collect())
            .unwrap_or_default();
        if has_persistent_storage {
            if !self.create_container_dirs(container_id, image_name) {
                container_id = 0;
            }
        }
        // else: skip ext2 dir creation — MemFs handles ephemeral storage

        let binary_vfs_path = format!("/var/images/{}{}", image_name, binary);

        // PRIORITY
        let priority = doc.table("scheduling")
            .and_then(|t| t.get_str("priority"))
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_PRIORITY);

        // ENDPOINT
        let endpoint_mode = doc.table("tokens")
            .and_then(|t| t.get_str("endpoint_mode"));
        let extra_token = match endpoint_mode {
            Some("listen") => {
                let ep = endpoint_create(self.token)?;
                token_derive(ep, Rights::IPC_RECV.bits() as usize, u64::MAX).unwrap_or(0)
            }
            Some("grantable") => {
                let ep = endpoint_create(self.token)?;
                let rights = Rights::IPC_RECV | Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT;
                token_derive(ep, rights.bits() as usize, u64::MAX).unwrap_or(0)
            }
            _ => 0,
        };

        // DEVICE (IRQ token)
        let devices: Vec<String> = doc.table("hardware")
            .and_then(|t| t.get_array("devices"))
            .map(|a| a.iter().map(|s| s.clone()).collect())
            .unwrap_or_default();
        let extra_token_1 = if devices.iter().any(|d| d == "irq") {
            token_derive(self.token, Rights::IRQ_HANDLE.bits() as usize, u64::MAX).unwrap_or(0)
        } else {
            0
        };

        // Build argv
        let mut argv_payload: Vec<u8> = Vec::new();
        argv_payload.extend_from_slice(binary.as_bytes());
        argv_payload.push(0);
        let argc = 1usize;

        let spawn_seq = self.next_spawn_seq();
        let spawn_start = self.clock_sample();

        // Instance params: console and vt share PARAM_CAP_PROFILE slot with their
        // instance ID, so we override after the profile is written.
        // Console also needs framebuffer params from /proc/fb.
        let mut overrides_buf: [(usize, u64); 8] = [(0, 0); 8];
        let mut n_overrides = 0;
        let mut fb_phys: u64 = 0;
        let mut fb_size: u64 = 0;
        if image_name == "console" {
            // Read framebuffer info from /proc/fb
            if let Some(data) = self.read_file_from_vfs("/proc/fb") {
                if let Ok(text) = core::str::from_utf8(&data) {
                    for line in text.lines() {
                        if let Some(v) = line.strip_prefix("phys=0x") {
                            fb_phys = u64::from_str_radix(v, 16).unwrap_or(0);
                        } else if let Some(v) = line.strip_prefix("size=") {
                            fb_size = v.parse::<u64>().unwrap_or(0);
                        } else if let Some(v) = line.strip_prefix("width=") {
                            let w = v.parse::<u64>().unwrap_or(0);
                            self.fb_width = w as u32;
                            overrides_buf[n_overrides] = (PARAM_FB_WIDTH, w);
                            n_overrides += 1;
                        } else if let Some(v) = line.strip_prefix("height=") {
                            let h = v.parse::<u64>().unwrap_or(0);
                            self.fb_height = h as u32;
                            overrides_buf[n_overrides] = (PARAM_FB_HEIGHT, h);
                            n_overrides += 1;
                        } else if let Some(v) = line.strip_prefix("pitch=") {
                            overrides_buf[n_overrides] = (PARAM_FB_PITCH, v.parse::<u64>().unwrap_or(0));
                            n_overrides += 1;
                        }
                    }
                }
            }
            overrides_buf[n_overrides] = (PARAM_FB_BASE, CONSOLE_FB_BASE as u64);
            n_overrides += 1;
            overrides_buf[n_overrides] = (PARAM_FB_PHYS, fb_phys);
            n_overrides += 1;
            overrides_buf[n_overrides] = (PARAM_FB_SIZE, fb_size);
            n_overrides += 1;
            overrides_buf[n_overrides] = (PARAM_CONSOLE_INSTANCE, 0);
            n_overrides += 1;
            overrides_buf[n_overrides] = (PARAM_CONSOLE_ACTIVE, 1);
            n_overrides += 1;
        }
        let param_overrides = &overrides_buf[..n_overrides];

        match self.spawn_service_with_env(
            &binary_vfs_path,
            priority,
            &argv_payload,
            argc,
            &[],
            0,
            0, // sender_tid=0 (internal autostart)
            spawn_seq,
            spawn_start,
            &[], // no FDAC
            requested_profile,
            extra_token,
            extra_token_1,
            param_overrides,
            None, // no caller view (internal autostart)
        ) {
            Ok((_thread_token, cookie, pid, _child_stdin_send)) => {
                let image_dir = format!("/var/images/{}", image_name);
                let mut view_mounts = default_view_for_profile(requested_profile);
                apply_image_dir_overrides(&mut view_mounts, image_name, &image_dirs);
                self.pid_to_container_id.insert(pid, container_id);
                self.container_owner_pids.insert(pid);
                self.register_vfs_view_for_thread(_thread_token, &view_mounts, requested_profile, container_id);
                self.pid_to_view.insert(pid, view_mounts);
                if image_name == "vtmgr" {
                    self.vtmgr_container_id = container_id;
                }
                self.container_instances.insert(container_id, ContainerInstance {
                    name: String::from(image_name),
                    container_id,
                    parent_container_id: 0, // autostart = top-level
                    pid,
                    image_path: image_dir,
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                });
                // Map framebuffer into console's address space.
                if image_name == "console" && fb_phys != 0 && fb_size != 0 {
                    if let Some(&space_tok) = self.cookie_to_space.get(&cookie) {
                        let num_pages = (fb_size as usize).div_ceil(PAGE_SIZE);
                        let _ = space_map_range(
                            space_tok,
                            CONSOLE_FB_BASE,
                            fb_phys as usize,
                            0x03 | MAP_DEVICE, // read + write + device
                            num_pages,
                            0,
                        );
                    }
                }
                let _ = debug_print(&format!(
                    "procmgr: autostart '{}' started pid={} cid={}",
                    image_name, pid, container_id
                ));
                let _ = cookie; // no exit notify for autostart
            }
            Err(err) => {
                let _ = debug_print(&format!(
                    "procmgr: autostart '{}' spawn failed: {:?}",
                    image_name, err
                ));
            }
        }
        Ok(())
    }

    fn poll_exit_notifications(&mut self) -> Result<()> {
        let registry_endpoint = registry::control_endpoint();
        let tokens = [
            self.exit_endpoint,
            self.spawn_endpoint,
            registry_endpoint,
            self.fault_endpoint,
        ];
        let _ = debug_print(&format!(
            "procmgr: poll tokens=[{},{},{},{}]",
            tokens[0], tokens[1], tokens[2], tokens[3]
        ));
        let mut buf = [0u8; 256];
        let (index, len, sender_tid) =
            match libcluu::syscall::ipc_recv_any_with_sender(&tokens, &mut buf, u64::MAX) {
                Ok(res) => {
                    let _ = debug_print(&format!(
                        "procmgr: recv_any idx={} len={} sender={}",
                        res.0, res.1, res.2
                    ));
                    res
                }
                Err(err) => {
                    let _ = debug_print(&format!("TRACE: exit recv failed {:?}", err));
                    return Ok(());
                }
            };
        if index == 3 {
            if let Some((msg, _payload)) = parse_message(&buf[..len]) {
                self.handle_fault_message(&msg);
            }
            return Ok(());
        }
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
        let mut container_id: u64 = 0;
        if let Some(pid) = self.cookie_to_pid.remove(&cookie) {
            let child_tid = self.pid_to_tid.get(&pid).copied().unwrap_or(0);
            // Extract container_id before clearing state for cleanup IPC.
            container_id = self.pid_to_container_id.remove(&pid).unwrap_or(0);
            self.clear_vfs_view_for_tid(child_tid);
            self.pid_to_cookie.remove(&pid);
            self.clear_pid_runtime_state(pid);
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
        if let Some(st) = self.cookie_to_space.remove(&cookie) {
            let _ = space_destroy(st);
        }
        // Revoke all derived tokens/endpoints created for this child
        if let Some(tokens) = self.cookie_to_tokens.remove(&cookie) {
            for tok in tokens {
                let _ = token_revoke(tok);
            }
        }
        // Clean up container dir only when no other process shares this container.
        if container_id > 0
            && !self.pid_to_container_id.values().any(|&cid| cid == container_id)
        {
            // Cascade: destroy child containers before cleaning up this container's storage
            self.destroy_container_children(container_id);
            self.container_instances.remove(&container_id);
            let _ = send_vfs_container_cleanup(self.vfs_endpoint, container_id, 1);
        }
        // HR4: Shell normal exit = explicit logout → session death.
        // Find session whose shell_cid matches the exiting container.
        let mut shell_session_cid = 0u64;
        for (&scid, session) in self.session_table.iter() {
            if session.shell_cid == container_id && container_id != 0 {
                shell_session_cid = scid;
                break;
            }
        }
        if shell_session_cid != 0 {
            // Cascade-destroy any remaining session children (su/sudo subshells)
            self.destroy_container_children(shell_session_cid);
            if let Some(session) = self.session_table.remove(&shell_session_cid) {
                let _ = debug_print(&format!(
                    "procmgr: session death (shell exit) user='{}' shell_cid={} session_cid={} vt={}",
                    session.username, container_id, shell_session_cid, session.vt_index
                ));
                if session.vt_index < VT_COUNT {
                    self.vt_to_session[session.vt_index] = 0;
                    let tty_ep = self.tty_endpoints[session.vt_index];
                    if tty_ep != 0 {
                        let death_msg = Message::new(
                            libcluu::ipc::PROCMGR_SESSION_DEATH_LABEL,
                            [session.vt_index, 0, 0, 0, 0, 0],
                            1,
                        );
                        let _ = send(tty_ep, &death_msg, IpcFlags::empty());
                    }
                }
                self.container_children.remove(&shell_session_cid);
            }
        }
        Ok(())
    }

    fn handle_registry_event(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            if let registry::RegistryEvent::Grant { service_name, name, token } = event {
                if name == "mounted" && !self.autostart_done {
                    let _ = debug_print(&format!(
                        "procmgr: VFS mounted signal received (token={})", token
                    ));
                    self.autostart_done = true;
                    self.run_autostart();
                    self.parse_users_toml();
                    self.try_auto_login();
                } else if name == "main" {
                    // Use service name to determine VT index (e.g., "tty:0" → 0).
                    if let Some(idx) = service_name.strip_prefix("tty:").and_then(|s| s.parse::<usize>().ok()) {
                        if idx < VT_COUNT {
                            self.tty_endpoints[idx] = token;
                            let _ =
                                debug_print(&format!("procmgr: tty:{} main granted {}", idx, token));
                            // H8: If a session already owns this VT, reattach it
                            // to the new TTY (crash recovery).
                            let session_cid = self.vt_to_session[idx];
                            if session_cid != 0 {
                                self.reattach_session_to_vt(idx, session_cid);
                            }
                            self.try_auto_login();
                        }
                    }
                }
            } else if let registry::RegistryEvent::SubscribeStatus { code } = event {
                if code != 0 {
                    // Reset all failed requested bits so we retry.
                    self.requested_tty_mask = 0;
                    // Re-mark already-subscribed ones.
                    for i in 0..VT_COUNT {
                        if self.tty_endpoints[i] != 0 {
                            self.requested_tty_mask |= 1u8 << i;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Handle a fault IPC from the kernel.
    ///
    /// Fault message format (label = 0xFA017):
    ///   words[0] = fault_type, words[1] = fault_addr, words[2] = error_code,
    ///   words[3] = rip, words[4] = thread_id, words[5] = reply_id
    fn handle_fault_message(&mut self, msg: &Message) {
        if msg.tag.label != PROCMGR_FAULT_LABEL {
            return;
        }
        let fault_type = msg.words[0];
        let fault_addr = msg.words[1];
        let rip = msg.words[3];
        let thread_id = msg.words[4];

        let _ = debug_print(&format!(
            "procmgr: FAULT type={} addr=0x{:x} rip=0x{:x} tid={}",
            fault_type, fault_addr, rip, thread_id
        ));

        // Look up the PID for the faulting thread and clean up as if it exited
        // with a fault signal (128 + signal convention).
        let fault_exit_code: i32 = -(fault_type as i32);
        if let Some(&pid) = self.tid_to_pid.get(&thread_id) {
            if let Some(&cookie) = self.pid_to_cookie.get(&pid) {
                // Reuse exit cleanup path: inject a synthetic exit event.
                // Remove exit_table entry for the thread token first.
                let thread_token = self.exit_table.remove(&cookie);

                let mut container_id: u64 = 0;
                if let Some(p) = self.cookie_to_pid.remove(&cookie) {
                    let child_tid = self.pid_to_tid.get(&p).copied().unwrap_or(0);
                    container_id = self.pid_to_container_id.remove(&p).unwrap_or(0);
                    self.clear_vfs_view_for_tid(child_tid);
                    self.pid_to_cookie.remove(&p);
                    self.clear_pid_runtime_state(p);
                    if let Some(owner_tid) = self.pid_owner_tid.remove(&p) {
                        self.on_child_reaped(owner_tid);
                    }
                }

                // Notify parent (e.g. shell) about the fault exit.
                if let Some(notify_endpoint) = self.exit_notify.remove(&cookie) {
                    let mut notify_msg = Message::new(PROCMGR_EXIT_LABEL, [0; 6], 2);
                    notify_msg.words[0] = cookie;
                    notify_msg.words[1] = fault_exit_code as usize;
                    let _ = send(notify_endpoint, &notify_msg, IpcFlags::empty());
                }

                let _ = debug_print(&format!(
                    "procmgr: fault exit pid={} cookie={} (code {})",
                    pid, cookie, fault_exit_code
                ));

                // Destroy the thread and address space.
                if let Some(tt) = thread_token {
                    let _ = thread_destroy(tt);
                }
                if let Some(st) = self.cookie_to_space.remove(&cookie) {
                    let _ = space_destroy(st);
                }
                if let Some(tokens) = self.cookie_to_tokens.remove(&cookie) {
                    for tok in tokens {
                        let _ = token_revoke(tok);
                    }
                }
                if container_id > 0
                    && !self.pid_to_container_id.values().any(|&cid| cid == container_id)
                {
                    self.destroy_container_children(container_id);
                    self.container_instances.remove(&container_id);
                    let _ = send_vfs_container_cleanup(self.vfs_endpoint, container_id, 1);
                }
                // HR3: Shell crash — clear shell from session but keep session alive.
                // No SESSION_DEATH sent; the session survives the crash.
                let mut crash_session_cid = 0u64;
                for (&scid, session) in self.session_table.iter_mut() {
                    if session.shell_cid == container_id && container_id != 0 {
                        let _ = debug_print(&format!(
                            "procmgr: shell crash user='{}' shell_cid={} (session persists)",
                            session.username, container_id
                        ));
                        session.shell_cid = 0;
                        session.stdin_endpoint = 0;
                        crash_session_cid = scid;
                        break;
                    }
                }
                // Remove stale shell entry from session's children list
                if crash_session_cid != 0 {
                    if let Some(children) = self.container_children.get_mut(&crash_session_cid) {
                        children.retain(|&cid| cid != container_id);
                    }
                }
            }
        }
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
        if msg.tag.label == PROCMGR_QUERY_CTTY_LABEL {
            return self.handle_ctty_query(msg, sender_tid);
        }
        if msg.tag.label == PROCMGR_SPAWN_SERVICE_LABEL {
            return self.handle_service_spawn(msg, payload);
        }
        if msg.tag.label == PROCMGR_CONTAINER_RUN_LABEL {
            return self.handle_container_run(msg, payload, sender_tid);
        }
        if msg.tag.label == PROCMGR_CONTAINER_LIST_LABEL {
            return self.handle_container_list(msg);
        }
        if msg.tag.label == libcluu::ipc::PROCMGR_SESSION_LOGIN_LABEL {
            return self.handle_session_login(msg, payload, sender_tid);
        }
        if msg.tag.label == libcluu::ipc::PROCMGR_ESCALATE_LABEL {
            return self.handle_escalate(msg, payload, sender_tid);
        }
        if msg.tag.label == libcluu::ipc::PROCMGR_SU_LABEL {
            return self.handle_su(msg, payload, sender_tid);
        }
        if msg.tag.label == PROCMGR_CONTAINER_STATS_LABEL {
            return self.handle_container_stats(msg, sender_tid);
        }
        self.handle_spawn_message(msg, payload, sender_tid)
    }

    fn build_session_view(&self, user_record: &UserRecord) -> ViewMountList {
        self.build_view_for_profile_and_home(user_record.profile, &user_record.home)
    }

    fn handle_session_login(&mut self, msg: &Message, payload: &[u8], _sender_tid: usize) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(libcluu::ipc::PROCMGR_SESSION_LOGIN_LABEL, [0; 6], 5);
        let vt_index = msg.words[1];

        // Parse username\0password\0 from payload
        let payload_str = core::str::from_utf8(payload).unwrap_or("");
        let mut parts = payload_str.splitn(3, '\0');
        let username = match parts.next() {
            Some(u) if !u.is_empty() => u,
            _ => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };
        let _password = parts.next().unwrap_or(""); // H13 will add verification

        // Look up user record
        let user_record = match self.user_records.get(username) {
            Some(r) => r,
            None => {
                let _ = debug_print(&format!("procmgr: login failed, unknown user '{}'", username));
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        let _ = debug_print(&format!("procmgr: session login user='{}' vt={}", username, vt_index));

        // Reject if a session is already active on this VT.
        if vt_index < VT_COUNT && self.vt_to_session[vt_index] != 0 {
            let _ = debug_print(&format!("procmgr: login rejected, vt={} has active session", vt_index));
            reply_msg.words[0] = Error::AlreadyExists.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // Clone fields before taking &mut self
        let profile = user_record.profile;
        let user_home = user_record.home.clone();
        let view_mounts = self.build_session_view(user_record);

        let tty_ep = if vt_index < VT_COUNT { self.tty_endpoints[vt_index] } else { self.tty_endpoints[0] };
        if tty_ep == 0 {
            let _ = debug_print(&format!("procmgr: login vt={}: no tty endpoint", vt_index));
            reply_msg.words[0] = Error::NotFound.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        let spawn_seq = self.next_spawn_seq();
        let spawn_start = self.clock_sample();
        let (shell_argv_payload, shell_argc) = build_shell_argv_payload(SHELL_AUTOSTART_CMD);
        let (user_env, user_envc) = build_user_env_payload(username, &user_home);

        // Temporarily wire stdout to target VT's tty
        let saved = self.tty_endpoints[0];
        self.tty_endpoints[0] = tty_ep;

        match self.spawn_service_with_env(
            SERVICE_PATH,
            DEFAULT_PRIORITY,
            &shell_argv_payload,
            shell_argc,
            &user_env,
            user_envc,
            1, // non-zero owner_tid to use caller_env_data
            spawn_seq,
            spawn_start,
            &[],
            profile,
            0,
            0,
            &[],
            None, // no caller view (session login uses SERVICE_PATH constant)
        ) {
            Ok((thread_token, _cookie, pid, stdin_send)) => {
                let session_cid = self.next_container_id();
                let shell_cid = self.next_container_id();
                self.pid_to_container_id.insert(pid, shell_cid);
                self.container_owner_pids.insert(pid);

                self.register_vfs_view_for_thread(thread_token, &view_mounts, profile, shell_cid);
                self.pid_to_view.insert(pid, view_mounts);

                self.container_instances.insert(shell_cid, ContainerInstance {
                    name: String::from("shell"),
                    container_id: shell_cid,
                    parent_container_id: session_cid,
                    pid,
                    image_path: String::from(SERVICE_PATH),
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                });

                self.container_children.entry(session_cid)
                    .or_insert_with(Vec::new).push(shell_cid);

                self.session_table.insert(session_cid, SessionEntry {
                    container_id: session_cid,
                    shell_cid,
                    pid,
                    username: String::from(username),
                    profile,
                    vt_index,
                    stdin_endpoint: stdin_send,
                });
                if vt_index < VT_COUNT {
                    self.vt_to_session[vt_index] = session_cid;
                }

                let _ = debug_print(&format!(
                    "procmgr: session created user='{}' pid={} session_cid={} shell_cid={} vt={}",
                    username, pid, session_cid, shell_cid, vt_index
                ));

                reply_msg.words[0] = 0;
                reply_msg.words[1] = session_cid as usize;
                reply_msg.words[2] = stdin_send;
            }
            Err(e) => {
                let _ = debug_print(&format!("procmgr: session spawn failed: {:?}", e));
                reply_msg.words[0] = e.to_errno() as usize;
            }
        }

        self.tty_endpoints[0] = saved;
        if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
        Ok(())
    }

    /// Handle PROCMGR_ESCALATE_LABEL: privilege escalation (sudo).
    ///
    /// Payload: password\0command\0
    /// Reply: words[0]=errno, words[1]=pid, words[2]=cookie, words[3]=stdin, words[4]=cid.
    fn handle_escalate(&mut self, msg: &Message, payload: &[u8], sender_tid: usize) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(libcluu::ipc::PROCMGR_ESCALATE_LABEL, [0; 6], 5);

        // Parse payload: password\0command\0
        let payload_str = core::str::from_utf8(payload).unwrap_or("");
        let mut parts = payload_str.splitn(3, '\0');
        let password = parts.next().unwrap_or("");
        let command_path = match parts.next() {
            Some(p) if !p.is_empty() => p,
            _ => {
                let _ = debug_print("procmgr: escalate rejected: missing command path");
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Resolve caller's session via sender_tid → session_table
        let (username, user_home) = match self.resolve_caller_session(sender_tid) {
            Some(session) => {
                let uname = session.username.clone();
                let home = self.user_records.get(&uname)
                    .map(|r| r.home.clone())
                    .unwrap_or_else(|| String::from("/tmp"));
                (uname, home)
            }
            None => {
                let _ = debug_print(&format!(
                    "procmgr: escalate rejected: sender_tid={} not in any session", sender_tid
                ));
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Look up user record, verify password, check escalation ceiling
        let record = match self.user_records.get(&username) {
            Some(r) => r,
            None => {
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };
        let password_ok = record.password.is_empty() || record.password == password;
        let escalate_profile = record.escalate;
        if !password_ok {
            let _ = debug_print(&format!(
                "procmgr: escalate rejected: bad password for '{}'", username
            ));
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }
        let escalate_profile = match escalate_profile {
            Some(profile) => profile,
            None => {
                let _ = debug_print(&format!(
                    "procmgr: escalate rejected: user '{}' has no escalate ceiling", username
                ));
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        let _ = debug_print(&format!(
            "procmgr: escalate user='{}' cmd='{}' profile={:#x}",
            username, command_path, escalate_profile.bits()
        ));

        // Build elevated view from escalation profile's default + user's home
        let view_mounts = self.build_view_for_profile_and_home(escalate_profile, &user_home);

        // Determine caller's container for parenting (cascading)
        let caller_pid = self.tid_to_pid.get(&sender_tid).copied().unwrap_or(0);
        let caller_container_id = self.pid_to_container_id.get(&caller_pid).copied().unwrap_or(0);

        // Check nesting depth
        if caller_container_id != 0 && self.container_depth(caller_container_id) >= MAX_NESTING_DEPTH {
            let _ = debug_print("procmgr: escalate rejected: nesting depth exceeded");
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // Resolve TTY for stdout wiring
        let caller_vt = self.resolve_caller_vt(sender_tid);
        let tty_ep = if caller_vt < VT_COUNT {
            self.tty_endpoints[caller_vt]
        } else {
            self.tty_endpoints[0]
        };

        // Resolve notify endpoint for exit notification
        let notify_endpoint = if msg.tag.words >= 2 { msg.words[1] } else { 0 };
        let notify_endpoint = match self.resolve_notify_endpoint(sender_tid, notify_endpoint) {
            Ok(ep) => ep,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Temporarily wire stdout to caller's VT tty
        let saved_tty = self.tty_endpoints[0];
        if tty_ep != 0 { self.tty_endpoints[0] = tty_ep; }

        let spawn_seq = self.next_spawn_seq();
        let spawn_start = self.clock_sample();

        // Build argv: use basename of command as argv[0]
        let basename = command_path.rsplit('/').next().unwrap_or(command_path);
        let mut argv_payload = Vec::new();
        argv_payload.extend_from_slice(basename.as_bytes());
        argv_payload.push(0);
        let argc = 1usize;
        let (esc_env, esc_envc) = build_user_env_payload(&username, &user_home);

        // Look up caller's VFS view for path resolution
        let caller_view_owned = self.pid_to_view.get(&caller_pid).cloned();

        match self.spawn_service_with_env(
            command_path,
            DEFAULT_PRIORITY,
            &argv_payload,
            argc,
            &esc_env,
            esc_envc,
            1,
            spawn_seq,
            spawn_start,
            &[],
            escalate_profile,
            0,
            0,
            &[],
            caller_view_owned.as_ref(),
        ) {
            Ok((thread_token, cookie, pid, stdin_send)) => {
                let container_id = self.next_container_id();
                self.pid_to_container_id.insert(pid, container_id);
                self.container_owner_pids.insert(pid);
                self.register_vfs_view_for_thread(thread_token, &view_mounts, escalate_profile, container_id);
                self.pid_to_view.insert(pid, view_mounts);

                self.container_instances.insert(container_id, ContainerInstance {
                    name: format!("sudo:{}", username),
                    container_id,
                    parent_container_id: caller_container_id,
                    pid,
                    image_path: String::from(command_path),
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                });

                // Track parent→child for cascading cleanup
                if caller_container_id != 0 {
                    self.container_children.entry(caller_container_id)
                        .or_insert_with(Vec::new).push(container_id);
                }

                // Wire exit notification
                if sender_tid != 0 {
                    let entry = self.sender_live_children.entry(sender_tid).or_insert(0);
                    *entry = entry.saturating_add(1);
                }
                if notify_endpoint != 0 {
                    self.exit_notify.insert(cookie, notify_endpoint);
                }

                reply_msg.words[0] = 0;
                reply_msg.words[1] = pid;
                reply_msg.words[2] = cookie;
                reply_msg.words[3] = stdin_send;
                reply_msg.words[4] = container_id as usize;
                let _ = debug_print(&format!(
                    "procmgr: escalate ok user='{}' pid={} cid={} profile={:#x}",
                    username, pid, container_id, escalate_profile.bits()
                ));
            }
            Err(err) => {
                let _ = debug_print(&format!("procmgr: escalate spawn failed: {:?}", err));
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }

        self.tty_endpoints[0] = saved_tty;
        if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
        Ok(())
    }

    /// Handle PROCMGR_SU_LABEL: identity switch (su).
    ///
    /// Payload: target_username\0password\0
    /// Reply: words[0]=errno, words[1]=pid, words[2]=cookie, words[3]=stdin, words[4]=cid.
    ///
    /// Creates a child container running target user's shell with target's profile
    /// and view. Does NOT update vt_to_session or session_table — this is a nested
    /// container, not a top-level session.
    fn handle_su(&mut self, msg: &Message, payload: &[u8], sender_tid: usize) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(libcluu::ipc::PROCMGR_SU_LABEL, [0; 6], 5);

        // Parse payload: target_username\0password\0
        let payload_str = core::str::from_utf8(payload).unwrap_or("");
        let mut parts = payload_str.splitn(3, '\0');
        let target_username = match parts.next() {
            Some(u) if !u.is_empty() => u,
            _ => {
                let _ = debug_print("procmgr: su rejected: missing target username");
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };
        let password = parts.next().unwrap_or("");

        // Look up target user record
        let (target_profile, target_home) = match self.user_records.get(target_username) {
            Some(record) => {
                // Verify target user's password
                let password_ok = record.password.is_empty() || record.password == password;
                if !password_ok {
                    let _ = debug_print(&format!(
                        "procmgr: su rejected: bad password for '{}'", target_username
                    ));
                    reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                    if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                    return Ok(());
                }
                (record.profile, record.home.clone())
            }
            None => {
                let _ = debug_print(&format!(
                    "procmgr: su rejected: unknown user '{}'", target_username
                ));
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Approach C: caller must strictly outrank target (capability narrowing only)
        let caller_pid = self.tid_to_pid.get(&sender_tid).copied().unwrap_or(0);
        let caller_profile = self.pid_to_profile.get(&caller_pid).copied()
            .unwrap_or(CapProfile::empty());
        if !caller_profile.can_grant(target_profile) || caller_profile == target_profile {
            let _ = debug_print(&format!(
                "procmgr: su rejected: caller profile {:#x} cannot narrow to target {:#x}",
                caller_profile.bits(), target_profile.bits()
            ));
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        let _ = debug_print(&format!(
            "procmgr: su target='{}' profile={:#x} (caller={:#x})",
            target_username, target_profile.bits(), caller_profile.bits()
        ));

        // Build view from target user's profile defaults + target's home
        let view_mounts = self.build_view_for_profile_and_home(target_profile, &target_home);

        // Determine caller's container for parenting (cascading)
        let caller_container_id = self.pid_to_container_id.get(&caller_pid).copied().unwrap_or(0);

        // Check nesting depth
        if caller_container_id != 0 && self.container_depth(caller_container_id) >= MAX_NESTING_DEPTH {
            let _ = debug_print("procmgr: su rejected: nesting depth exceeded");
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // Resolve TTY for stdout wiring
        let caller_vt = self.resolve_caller_vt(sender_tid);
        let tty_ep = if caller_vt < VT_COUNT {
            self.tty_endpoints[caller_vt]
        } else {
            self.tty_endpoints[0]
        };

        // Resolve notify endpoint for exit notification
        let notify_endpoint = if msg.tag.words >= 2 { msg.words[1] } else { 0 };
        let notify_endpoint = match self.resolve_notify_endpoint(sender_tid, notify_endpoint) {
            Ok(ep) => ep,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Temporarily wire stdout to caller's VT tty
        let saved_tty = self.tty_endpoints[0];
        if tty_ep != 0 { self.tty_endpoints[0] = tty_ep; }

        let spawn_seq = self.next_spawn_seq();
        let spawn_start = self.clock_sample();
        let (shell_argv_payload, shell_argc) = build_shell_argv_payload("");
        let (su_env, su_envc) = build_user_env_payload(target_username, &target_home);

        match self.spawn_service_with_env(
            SERVICE_PATH,
            DEFAULT_PRIORITY,
            &shell_argv_payload,
            shell_argc,
            &su_env,
            su_envc,
            1,
            spawn_seq,
            spawn_start,
            &[],
            target_profile,
            0,
            0,
            &[],
            None, // no caller view (su uses SERVICE_PATH constant)
        ) {
            Ok((thread_token, cookie, pid, stdin_send)) => {
                let container_id = self.next_container_id();
                self.pid_to_container_id.insert(pid, container_id);
                self.container_owner_pids.insert(pid);
                self.register_vfs_view_for_thread(thread_token, &view_mounts, target_profile, container_id);
                self.pid_to_view.insert(pid, view_mounts);

                self.container_instances.insert(container_id, ContainerInstance {
                    name: format!("su:{}", target_username),
                    container_id,
                    parent_container_id: caller_container_id,
                    pid,
                    image_path: String::from(SERVICE_PATH),
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                });

                // Track parent→child for cascading cleanup
                if caller_container_id != 0 {
                    self.container_children.entry(caller_container_id)
                        .or_insert_with(Vec::new).push(container_id);
                }

                // Wire exit notification
                if sender_tid != 0 {
                    let entry = self.sender_live_children.entry(sender_tid).or_insert(0);
                    *entry = entry.saturating_add(1);
                }
                if notify_endpoint != 0 {
                    self.exit_notify.insert(cookie, notify_endpoint);
                }

                reply_msg.words[0] = 0;
                reply_msg.words[1] = pid;
                reply_msg.words[2] = cookie;
                reply_msg.words[3] = stdin_send;
                reply_msg.words[4] = container_id as usize;
                let _ = debug_print(&format!(
                    "procmgr: su ok target='{}' pid={} cid={} profile={:#x}",
                    target_username, pid, container_id, target_profile.bits()
                ));
            }
            Err(err) => {
                let _ = debug_print(&format!("procmgr: su spawn failed: {:?}", err));
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }

        self.tty_endpoints[0] = saved_tty;
        if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
        Ok(())
    }

    /// Handle PROCMGR_CONTAINER_STATS_LABEL: return 64-byte fixed records for
    /// each container visible to the caller.
    fn handle_container_stats(&self, msg: &Message, sender_tid: usize) -> Result<()> {
        let reply_token = match extract_reply_id(msg) {
            Some(t) => t,
            None => return Ok(()),
        };

        // Determine caller profile for visibility filtering.
        // Admin access is granted if either the container profile OR the owning
        // session profile carries the ADMIN capability (root sessions use ADMIN).
        let caller_pid = self.tid_to_pid.get(&sender_tid).copied().unwrap_or(0);
        let caller_profile = self.pid_to_profile.get(&caller_pid).copied()
            .unwrap_or(CapProfile::empty());
        let session_profile = self.resolve_caller_session(sender_tid)
            .map(|s| s.profile)
            .unwrap_or(CapProfile::empty());
        let is_admin = caller_profile.contains(CapProfile::ADMIN)
            || session_profile.contains(CapProfile::ADMIN);

        // Find caller's session for subtree filtering
        let caller_session_cid = if !is_admin {
            self.resolve_caller_session(sender_tid)
                .map(|s| s.container_id)
                .unwrap_or(0)
        } else {
            0
        };

        // Collect visible containers + session entries
        let mut records: Vec<u8> = Vec::new();
        let mut record_count: usize = 0;
        let total_containers = self.container_instances.len();
        let total_sessions = self.session_table.len();

        // Helper: check if cid is in session's subtree
        let in_subtree = |cid: u64| -> bool {
            if is_admin { return true; }
            if caller_session_cid == 0 { return false; }
            let mut walk = cid;
            while walk != 0 {
                if walk == caller_session_cid { return true; }
                match self.container_instances.get(&walk) {
                    Some(inst) => walk = inst.parent_container_id,
                    None => break,
                }
            }
            false
        };

        // Emit session entries (virtual containers, state=3)
        for (&scid, session) in &self.session_table {
            if !is_admin && scid != caller_session_cid { continue; }
            let mut rec = [0u8; 64];
            rec[0..8].copy_from_slice(&scid.to_le_bytes());
            // parent_container_id = 0 for sessions
            // pid = 0 for virtual sessions
            // profile
            rec[24..26].copy_from_slice(&session.profile.bits().to_le_bytes());
            rec[26] = 3; // state: session-only
            rec[27] = session.vt_index as u8;
            // mapped_pages = 0 for virtual sessions
            // cpu_ticks = 0 for virtual sessions
            let name = format!("session:{}", session.username);
            let name_bytes = name.as_bytes();
            let name_len = name_bytes.len().min(24);
            rec[40..40 + name_len].copy_from_slice(&name_bytes[..name_len]);
            records.extend_from_slice(&rec);
            record_count += 1;
        }

        // Emit container instances
        for (&cid, inst) in &self.container_instances {
            if !in_subtree(cid) { continue; }
            let mut rec = [0u8; 64];
            rec[0..8].copy_from_slice(&cid.to_le_bytes());
            rec[8..16].copy_from_slice(&inst.parent_container_id.to_le_bytes());
            rec[16..24].copy_from_slice(&(inst.pid as u64).to_le_bytes());
            let profile = self.pid_to_profile.get(&inst.pid).copied()
                .unwrap_or(CapProfile::empty());
            rec[24..26].copy_from_slice(&profile.bits().to_le_bytes());
            // State: 0=running if pid has a thread, 2=dead otherwise
            let state = if inst.pid != 0 && self.pid_to_tid.get(&inst.pid).is_some() { 0u8 } else { 2u8 };
            rec[26] = state;
            // VT: find via session parent chain
            let vt = {
                let mut walk = inst.parent_container_id;
                let mut found_vt = 0xFFu8;
                while walk != 0 {
                    if let Some(session) = self.session_table.get(&walk) {
                        found_vt = session.vt_index as u8;
                        break;
                    }
                    match self.container_instances.get(&walk) {
                        Some(p) => walk = p.parent_container_id,
                        None => break,
                    }
                }
                found_vt
            };
            rec[27] = vt;
            // Memory: query real mapped page counts from kernel (heap u16 + code+stack u16)
            let (cpu_ticks, heap_pages, other_pages): (u64, u16, u16) = if inst.pid != 0 {
                if let Some(&cookie) = self.pid_to_cookie.get(&inst.pid) {
                    let ticks = self.exit_table.get(&cookie)
                        .map(|&tt| libcluu::syscall::thread_get_stats(tt).unwrap_or(0))
                        .unwrap_or(0);
                    let (code, heap, stack) = self.cookie_to_space.get(&cookie)
                        .map(|&sp| libcluu::syscall::space_get_stats(sp).unwrap_or((0, 0, 0)))
                        .unwrap_or((0, 0, 0));
                    (ticks, heap, code.saturating_add(stack))
                } else { (0, 0, 0) }
            } else { (0, 0, 0) };
            rec[28..30].copy_from_slice(&heap_pages.to_le_bytes());
            rec[30..32].copy_from_slice(&other_pages.to_le_bytes());
            rec[32..40].copy_from_slice(&cpu_ticks.to_le_bytes());
            let name_bytes = inst.name.as_bytes();
            let name_len = name_bytes.len().min(24);
            rec[40..40 + name_len].copy_from_slice(&name_bytes[..name_len]);
            records.extend_from_slice(&rec);
            record_count += 1;
        }

        let mut reply_msg = Message::new(PROCMGR_CONTAINER_STATS_LABEL, [0; 6], 4);
        // words[0] is overwritten by reply_with_payload to payload length
        reply_msg.words[1] = record_count;
        reply_msg.words[2] = total_containers;
        reply_msg.words[3] = total_sessions;

        let _ = ipc::reply_with_payload(reply_token, &reply_msg, &records);

        Ok(())
    }

    /// Handle PROCMGR_QUERY_CTTY_LABEL: reply with the caller's controlling terminal index.
    fn handle_ctty_query(&self, msg: &Message, sender_tid: usize) -> Result<()> {
        let reply_token = match extract_reply_id(msg) {
            Some(t) => t,
            None => return Ok(()),
        };

        // Look up pid by sender_tid, then look up ctty.
        let ctty_index = self
            .pid_owner_tid
            .iter()
            .find(|(_, &tid)| tid == sender_tid)
            .and_then(|(&pid, _)| self.pid_ctty.get(&pid))
            .copied()
            .unwrap_or(0u8);

        let reply_msg = Message::new(
            PROCMGR_QUERY_CTTY_LABEL,
            [ctty_index as usize, 0, 0, 0, 0, 0],
            1,
        );
        let _ = libcluu::ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        Ok(())
    }

    /// Handle PROCMGR_SPAWN_SERVICE_LABEL: generic system service spawn.
    ///
    /// The caller specifies path, priority, token mode, and param overrides.
    /// Procmgr validates the request, creates the process, and applies the
    /// wiring without any service-specific knowledge.
    ///
    /// Policy enforcement:
    /// - Only initrd paths (sys/*) are permitted for service spawns.
    /// - Param indices must be within bounds (0-9).
    /// - Token mode must be a valid enum value (0-2).
    /// - The spawn endpoint itself is capability-gated (only holders can call).
    fn handle_service_spawn(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        // words[0] = payload length (used by parse_message), metadata in words[1..3]
        let priority = msg.words[1];
        let token_extra_mode = msg.words[2]; // 0=none, 1=listen, 2=grantable
        let param_count = msg.words[3];

        // ── Policy: read requested CapProfile from words[4] ──
        // Default to SERVICE when 0 (backward compat: existing callers
        // like vtmgr don't send this yet).
        let requested_profile = {
            let raw = msg.words[4] as u16;
            if raw == 0 {
                CapProfile::SERVICE
            } else {
                CapProfile::from_bits_truncate(raw)
            }
        };

        // ── Policy: cap at SERVICE ceiling ──
        // Service spawn path: holding the spawn endpoint IS the authorization.
        // No caller lookup needed — just ensure nothing exceeds SERVICE.
        if !CapProfile::SERVICE.can_grant(requested_profile) {
            let _ = debug_print("procmgr: service spawn rejected: exceeds SERVICE ceiling");
            return Ok(());
        }

        // ── Policy: validate token mode ──
        if token_extra_mode > 2 {
            let _ = debug_print("procmgr: service spawn rejected: invalid token mode");
            return Ok(());
        }

        // ── Policy: validate param count bounds ──
        if param_count > 10 {
            let _ = debug_print("procmgr: service spawn rejected: too many params");
            return Ok(());
        }

        // Parse path from payload.
        let path = match parse_cstr(payload) {
            Some(p) => p,
            None => {
                let _ = debug_print("procmgr: service spawn rejected: no path");
                return Ok(());
            }
        };

        // ── Policy: only initrd paths are permitted for service spawns ──
        if !path.starts_with("sys/") {
            let _ = debug_print(&format!(
                "procmgr: service spawn rejected: path '{}' not permitted",
                path
            ));
            return Ok(());
        }

        // Parse param overrides from payload (after path\0).
        let path_nul_end = payload
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(payload.len())
            + 1;
        let param_data = &payload[path_nul_end..];
        let mut params = [0u64; 10];
        for i in 0..param_count {
            let offset = i * 10; // 2 bytes index + 8 bytes value
            if offset + 10 > param_data.len() {
                let _ = debug_print("procmgr: service spawn: param data truncated");
                break;
            }
            let idx = u16::from_le_bytes([param_data[offset], param_data[offset + 1]]) as usize;

            // ── Policy: validate param index bounds ──
            if idx >= 10 {
                let _ = debug_print(&format!(
                    "procmgr: service spawn rejected: param index {} out of range",
                    idx
                ));
                return Ok(());
            }
            let val = u64::from_le_bytes([
                param_data[offset + 2],
                param_data[offset + 3],
                param_data[offset + 4],
                param_data[offset + 5],
                param_data[offset + 6],
                param_data[offset + 7],
                param_data[offset + 8],
                param_data[offset + 9],
            ]);
            params[idx] = val;
        }

        // NOTE: PARAM_CAP_PROFILE (slot 5) intentionally NOT written here.
        // Cap profile is enforced server-side via pid_to_profile, and slot 5
        // is shared with PARAM_CONSOLE_INSTANCE for console services.

        let _ = debug_print(&format!(
            "procmgr: service spawn path='{}' pri={} mode={}",
            path, priority, token_extra_mode
        ));

        // Load and parse ELF from initrd.
        let initrd =
            unsafe { core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, self.initrd_size) };
        let service_bytes = find_member(initrd, path).ok_or(Error::NotFound)?;
        let elf = ElfFile::parse(service_bytes)?;

        let space_token = space_create(self.token)?;
        libcluu::map_segments(space_token, &elf, service_bytes)?;
        libcluu::map_stack(
            space_token,
            SERVICE_STACK_TOP,
            SERVICE_STACK_SIZE,
            STACK_FLAGS,
        )?;

        // Build tokens (standard layout for system services).
        let slot_rights = profile_to_rights(requested_profile);
        let mut tokens = [0usize; 16];
        tokens[TOKEN_STDIN] = endpoint_create(self.token)?;
        tokens[TOKEN_STDOUT] = endpoint_create(self.token)?;
        tokens[TOKEN_STDERR] = endpoint_create(self.token)?;
        tokens[TOKEN_STDLOG] = endpoint_create(self.token)?;
        tokens[TOKEN_SELF] = derive_slot(self.token, slot_rights[TOKEN_SELF])?;
        tokens[TOKEN_SPACE] = derive_slot(space_token, slot_rights[TOKEN_SPACE])?;
        tokens[TOKEN_IPC] = derive_slot(self.token, slot_rights[TOKEN_IPC])?;
        tokens[TOKEN_CLOCK] = self.clock_token;
        tokens[TOKEN_REGISTRY] = self.registry_send;

        // Apply TOKEN_EXTRA_0 based on requested mode.
        match token_extra_mode {
            1 => {
                // Listen-only endpoint (recv only).
                let ep = endpoint_create(self.token)?;
                tokens[TOKEN_EXTRA_0] =
                    token_derive(ep, Rights::IPC_RECV.bits() as usize, u64::MAX)?;
            }
            2 => {
                // Grantable endpoint (recv + send + call + grant).
                let ep = endpoint_create(self.token)?;
                let rights = Rights::IPC_RECV | Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT;
                tokens[TOKEN_EXTRA_0] = token_derive(ep, rights.bits() as usize, u64::MAX)?;
            }
            _ => {} // No TOKEN_EXTRA_0
        }

        // Map ProcessInfo (system service: no exit tracking, no argv).
        let info = ProcessInfo {
            exit_token: 0,
            exit_cookie: 0,
            pid: 0,
            tokens,
            params,
        };
        let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);
        let info_offset = PROCESS_INFO_ADDR - page_base;
        let info_size = size_of::<ProcessInfo>();
        let mut page = [0u8; PAGE_SIZE];
        let bytes = unsafe {
            core::slice::from_raw_parts(&info as *const ProcessInfo as *const u8, info_size)
        };
        let end = info_offset + bytes.len();
        if end > PAGE_SIZE {
            return Err(Error::InvalidArgument);
        }
        page[info_offset..end].copy_from_slice(bytes);
        space_map(
            space_token,
            page_base,
            page.as_ptr() as usize,
            0x01,
            PAGE_SIZE,
        )?;

        let thread_token = thread_create(
            space_token,
            elf.entry_point as usize,
            SERVICE_STACK_TOP,
            priority,
        )?;

        // Register VFS view for the service based on its profile.
        // System services (pid=0) don't get private storage — container_id=0.
        let view_mounts = default_view_for_profile(requested_profile);
        self.register_vfs_view_for_thread(thread_token, &view_mounts, requested_profile, 0);

        let _ = debug_print(&format!("procmgr: service '{}' spawned", path));
        Ok(())
    }

    fn handle_spawn_message(
        &mut self,
        msg: &Message,
        payload: &[u8],
        sender_tid: usize,
    ) -> Result<()> {
        let spawn_seq = self.next_spawn_seq();
        let spawn_start = self.clock_sample();
        let mut reply_msg = Message::new(PROCMGR_SPAWN_LABEL, [0; 6], 4);
        let reply_token = extract_reply_id(msg);

        // Only containers with SPAWN capability may use bare spawn (for child
        // processes like benchprobe→noop or spawnpipeprobe→pipe child).
        let (caller_pid, caller_profile, caller_container_id) = match self
            .tid_to_pid
            .get(&sender_tid)
            .copied()
        {
            Some(pid) => {
                let profile = self.pid_to_profile.get(&pid).copied().unwrap_or(CapProfile::empty());
                let cid = self.pid_to_container_id.get(&pid).copied().unwrap_or(0);
                (pid, profile, cid)
            }
            None => {
                let _ = debug_print(&format!(
                    "procmgr: bare spawn rejected unknown sender_tid={}",
                    sender_tid
                ));
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                let _ = self.send_spawn_reply(reply_token, &reply_msg);
                return Ok(());
            }
        };
        if caller_container_id == 0 || !caller_profile.contains(CapProfile::SPAWN) {
            let _ = debug_print(&format!(
                "procmgr: bare spawn rejected sender_tid={} (no container or no SPAWN cap)",
                sender_tid
            ));
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            let _ = self.send_spawn_reply(reply_token, &reply_msg);
            return Ok(());
        }

        self.log_spawn_stage(spawn_seq, "spawn_request", spawn_start);
        let _ = debug_print(&format!(
            "procmgr: spawn request sender_tid={} words={}",
            sender_tid, msg.tag.words
        ));
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
                let _ = debug_print("procmgr: spawn request missing path");
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                let _ = self.send_spawn_reply(reply_token, &reply_msg);
                return Ok(());
            }
        };
        let _ = debug_print(&format!("procmgr: spawn path {}", path));

        if !path.starts_with('/') {
            let _ = debug_print(&format!("procmgr: rejecting relative path '{}'", path));
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            let _ = self.send_spawn_reply(reply_token, &reply_msg);
            return Ok(());
        }

        // Inherit parent's view and profile.
        let child_view_mounts = match self.pid_to_view.get(&caller_pid) {
            Some(view) => view.clone(),
            None => default_view_for_profile(caller_profile),
        };
        let child_profile = caller_profile;

        let priority = DEFAULT_PRIORITY;

        // Extract argv data: payload is [path\0, argv[0]\0, argv[1]\0, ...]
        let argc = if msg.tag.words >= 2 { msg.words[1] } else { 0 };
        let fdac_offset = if msg.tag.words >= 3 { msg.words[2] } else { 0 };
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
        let fdac_data = if fdac_offset > 0 && fdac_offset < payload.len() {
            &payload[fdac_offset..]
        } else {
            &[]
        };

        match self.spawn_service_with_env(
            path,
            priority,
            argv_data,
            argc,
            &[],
            0,
            sender_tid,
            spawn_seq,
            spawn_start,
            fdac_data,
            child_profile,
            0,
            0,
            &[],
            Some(&child_view_mounts),
        ) {
            Ok((thread_token, cookie, pid, child_stdin_send)) => {
                reply_msg.words[0] = 0;
                reply_msg.words[1] = pid;
                reply_msg.words[2] = cookie;
                reply_msg.words[3] = child_stdin_send;
                // Inherit parent's container.
                self.pid_to_container_id.insert(pid, caller_container_id);
                self.register_vfs_view_for_thread(thread_token, &child_view_mounts, child_profile, caller_container_id);
                self.pid_to_view.insert(pid, child_view_mounts.clone());
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
        } else {
            self.log_spawn_stage(spawn_seq, "reply_sent", spawn_start);
        }
        Ok(())
    }

    fn resolve_notify_endpoint(
        &mut self,
        sender_tid: usize,
        requested_notify_endpoint: usize,
    ) -> Result<usize> {
        if sender_tid == 0 {
            if requested_notify_endpoint != 0 {
                let _ = debug_print(
                    "procmgr: deny notify endpoint bind for unauthenticated sender_tid=0",
                );
                return Err(Error::PermissionDenied);
            }
            return Ok(0);
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

    fn clear_pid_runtime_state(&mut self, pid: usize) {
        self.pid_to_profile.remove(&pid);
        self.pid_to_view.remove(&pid);
        self.container_owner_pids.remove(&pid);
        if let Some(thread_tid) = self.pid_to_tid.remove(&pid) {
            self.tid_to_pid.remove(&thread_tid);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_service(
        &mut self,
        path: &str,
        priority: usize,
        argv_payload: &[u8],
        argc: usize,
        owner_tid: usize,
        spawn_seq: usize,
        spawn_start: u64,
        profile: CapProfile,
    ) -> Result<(usize, usize, usize, usize)> {
        self.spawn_service_with_env(
            path,
            priority,
            argv_payload,
            argc,
            &[],
            0,
            owner_tid,
            spawn_seq,
            spawn_start,
            &[],
            profile,
            0,
            0,
            &[],
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_service_with_env(
        &mut self,
        path: &str,
        priority: usize,
        argv_payload: &[u8],
        argc: usize,
        caller_env_data: &[u8],
        caller_envc: usize,
        owner_tid: usize,
        spawn_seq: usize,
        spawn_start: u64,
        fdac_data: &[u8],
        profile: CapProfile,
        extra_token: usize,
        extra_token_1: usize,
        param_overrides: &[(usize, u64)],
        caller_view: Option<&ViewMountList>,
    ) -> Result<(usize, usize, usize, usize)> {
        // Build env data: for bootstrap (owner_tid==0) use defaults,
        // otherwise use caller-provided env (from posix_spawn)
        let (env_data, envc) = if owner_tid == 0 {
            build_default_env_payload()
        } else if caller_envc > 0 && !caller_env_data.is_empty() {
            (caller_env_data.to_vec(), caller_envc)
        } else {
            // Inherit default env if caller didn't provide any
            build_default_env_payload()
        };
        debug_print("Creating address space...")?;
        let space_token = space_create(self.token)?;
        debug_print(&format!("Address space created: {}", space_token))?;
        self.log_spawn_stage(spawn_seq, "space_create_done", spawn_start);

        let mut entry_point = 0usize;
        let mut mapped = false;

        if !path.starts_with("/dev/initrd/") {
            self.log_spawn_stage(spawn_seq, "elf_fetch_start", spawn_start);
            if let Ok(Some(entry)) = self.map_elf_from_vfs(path, space_token, caller_view) {
                entry_point = entry;
                mapped = true;
                debug_print(&format!("Mapped ELF from VFS (entry=0x{:x})", entry_point))?;
                self.log_spawn_stage(spawn_seq, "elf_fetch_done", spawn_start);
            }
        }

        if !mapped {
            // Fall back to loading bytes in-process.
            self.log_spawn_stage(spawn_seq, "elf_fetch_start", spawn_start);
            let (elf_data, from_vfs) = self.load_elf(path, caller_view)?;
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
            self.log_spawn_stage(spawn_seq, "elf_fetch_done", spawn_start);
            self.log_spawn_stage(spawn_seq, "map_segments_done", spawn_start);
        } else {
            debug_print("ELF segments mapped")?;
            self.log_spawn_stage(spawn_seq, "map_segments_done", spawn_start);
        }

        debug_print("Mapping stack...")?;
        libcluu::map_stack(
            space_token,
            SERVICE_STACK_TOP,
            SERVICE_STACK_SIZE,
            STACK_FLAGS,
        )?;
        debug_print("Stack mapped")?;
        self.log_spawn_stage(spawn_seq, "stack_map_done", spawn_start);

        let send_rights = Rights::IPC_SEND.bits() as usize;
        let child_endpoint = token_derive(self.exit_endpoint, send_rights, u64::MAX)?;
        let cookie = self.next_exit_cookie();
        let pid = self.next_pid();
        debug_print(&format!(
            "TRACE: child exit ep {} cookie {} pid {}",
            child_endpoint, cookie, pid
        ))?;
        let stdin_endpoint = endpoint_create(self.token)?;
        let (stdout_endpoint, stderr_endpoint, stdlog_endpoint) = if self.tty_endpoints[0] != 0 {
            // The tty main endpoint already grants IPC_SEND, so reuse it directly.
            (
                self.tty_endpoints[0],
                self.tty_endpoints[0],
                self.tty_endpoints[0],
            )
        } else {
            (
                endpoint_create(self.token)?,
                endpoint_create(self.token)?,
                endpoint_create(self.token)?,
            )
        };
        // Derive capability tokens based on the child's CapProfile.
        let slot_rights = profile_to_rights(profile);
        let proc_cap = derive_slot(self.token, slot_rights[TOKEN_IPC])?;
        let self_cap = derive_slot(self.token, slot_rights[TOKEN_SELF])?;
        let child_space_token = derive_slot(space_token, slot_rights[TOKEN_SPACE])?;
        // Parse FDAC (fd actions) to override child stdio endpoints
        let mut pipe_mask: u8 = 0;
        let mut stdin_ep = stdin_endpoint;
        let mut stdout_ep = stdout_endpoint;
        let mut stderr_ep = stderr_endpoint;
        let mut stdlog_ep = stdlog_endpoint;

        if fdac_data.len() >= 8 {
            let magic =
                u32::from_le_bytes([fdac_data[0], fdac_data[1], fdac_data[2], fdac_data[3]]);
            let count = u32::from_le_bytes([fdac_data[4], fdac_data[5], fdac_data[6], fdac_data[7]])
                as usize;
            if magic == 0x46444143 && count <= 4 {
                // Each FdAction is 16 bytes: u32 target_fd + u32 flags + usize endpoint
                for i in 0..count {
                    let base = 8 + i * 16;
                    if base + 16 > fdac_data.len() {
                        break;
                    }
                    let target_fd = u32::from_le_bytes([
                        fdac_data[base],
                        fdac_data[base + 1],
                        fdac_data[base + 2],
                        fdac_data[base + 3],
                    ]);
                    let flags = u32::from_le_bytes([
                        fdac_data[base + 4],
                        fdac_data[base + 5],
                        fdac_data[base + 6],
                        fdac_data[base + 7],
                    ]);
                    let endpoint = usize::from_le_bytes([
                        fdac_data[base + 8],
                        fdac_data[base + 9],
                        fdac_data[base + 10],
                        fdac_data[base + 11],
                        fdac_data[base + 12],
                        fdac_data[base + 13],
                        fdac_data[base + 14],
                        fdac_data[base + 15],
                    ]);

                    let is_pipe = (flags & 0x01) != 0;
                    // Validate + narrow: stdin needs recv, others need send.
                    let probe_rights = match target_fd {
                        0 => Rights::IPC_RECV.bits() as usize,
                        _ => Rights::IPC_SEND.bits() as usize,
                    };
                    match token_derive(endpoint, probe_rights, u64::MAX) {
                        Ok(derived) => {
                            match target_fd {
                                0 => {
                                    stdin_ep = derived;
                                    if is_pipe { pipe_mask |= 1 << 0; }
                                }
                                1 => {
                                    stdout_ep = derived;
                                    if is_pipe { pipe_mask |= 1 << 1; }
                                }
                                2 => {
                                    stderr_ep = derived;
                                    if is_pipe { pipe_mask |= 1 << 2; }
                                }
                                3 => {
                                    stdlog_ep = derived;
                                    if is_pipe { pipe_mask |= 1 << 3; }
                                }
                                _ => {}
                            }
                        }
                        Err(_) => {
                            let _ = debug_print(&format!(
                                "procmgr: FDAC rejected: endpoint {} for fd {} failed derive",
                                endpoint, target_fd
                            ));
                            return Err(Error::PermissionDenied);
                        }
                    }
                }
                let _ = debug_print(&format!(
                    "procmgr: FDAC parsed {} actions, pipe_mask=0x{:02x}",
                    count, pipe_mask
                ));
            }
        }

        let parent_stdin_send =
            match token_derive(stdin_ep, Rights::IPC_SEND.bits() as usize, u64::MAX) {
                Ok(token) => token,
                Err(_) => 0, // No access rather than raw endpoint on derivation failure
            };

        // Inject framebuffer dimensions as defaults so all processes can compute
        // terminal cols/rows.  Caller overrides are applied after, so e.g.
        // the console service can still override with its explicit values.
        let mut eo_buf = [(0usize, 0u64); 14];
        let mut n_eo = 0usize;
        if self.fb_width != 0 {
            eo_buf[n_eo] = (PARAM_FB_WIDTH, self.fb_width as u64);
            n_eo += 1;
            eo_buf[n_eo] = (PARAM_FB_HEIGHT, self.fb_height as u64);
            n_eo += 1;
        }
        for &(idx, val) in param_overrides.iter().take(12) {
            eo_buf[n_eo] = (idx, val);
            n_eo += 1;
        }
        let effective_overrides = &eo_buf[..n_eo];

        map_process_info_page(
            space_token,
            child_endpoint,
            cookie,
            pid,
            stdin_ep,
            stdout_ep,
            stderr_ep,
            stdlog_ep,
            self.registry_send,
            proc_cap,
            self_cap,
            child_space_token,
            self.clock_token,
            argv_payload,
            argc,
            &env_data,
            envc,
            pipe_mask,
            profile,
            extra_token,
            extra_token_1,
            effective_overrides,
        )?;

        let thread_token = thread_create(space_token, entry_point, SERVICE_STACK_TOP, priority)?;
        // Set fault endpoint so the kernel forwards faults to us instead of silently killing.
        if self.fault_endpoint != 0 {
            if let Err(err) = thread_set_fault_endpoint(thread_token, self.fault_endpoint) {
                let _ = debug_print(&format!(
                    "procmgr: thread_set_fault_endpoint failed token={} ep={} err={:?}",
                    thread_token, self.fault_endpoint, err
                ));
            }
        }
        let thread_tid = thread_get_id(thread_token)?;
        self.log_spawn_stage(spawn_seq, "thread_start_done", spawn_start);

        self.exit_table.insert(cookie, thread_token);
        self.pid_to_cookie.insert(pid, cookie);
        self.cookie_to_pid.insert(cookie, pid);
        self.pid_owner_tid.insert(pid, owner_tid);
        self.pid_to_tid.insert(pid, thread_tid);
        self.tid_to_pid.insert(thread_tid, pid);
        self.pid_to_profile.insert(pid, profile);
        self.cookie_to_space.insert(cookie, space_token);
        // Track derived tokens/endpoints for cleanup on exit (skip 0-value slots)
        let mut derived_tokens: Vec<usize> = [
            child_endpoint,
            stdin_ep,
            proc_cap,
            self_cap,
            child_space_token,
        ]
        .into_iter()
        .filter(|&t| t != 0)
        .collect();
        if parent_stdin_send != 0 && parent_stdin_send != stdin_ep {
            derived_tokens.push(parent_stdin_send);
        }
        self.cookie_to_tokens.insert(cookie, derived_tokens);
        Ok((thread_token, cookie, pid, parent_stdin_send))
    }

    /// Load ELF data from VFS or initrd.
    /// Returns (data, from_vfs) where from_vfs indicates the source.
    /// Path must be absolute (start with '/').
    /// Initrd is only accessible via /dev/initrd/ prefix.
    fn load_elf(&mut self, path: &str, caller_view: Option<&ViewMountList>) -> Result<(Vec<u8>, bool)> {
        const INITRD_PREFIX: &str = "/dev/initrd/";

        // Check if path is for initrd (system paths bypass view resolution)
        if let Some(initrd_path) = path.strip_prefix(INITRD_PREFIX) {
            let initrd = unsafe {
                core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, self.initrd_size)
            };
            if let Some(bytes) = find_member(initrd, initrd_path) {
                return Ok((bytes.to_vec(), false));
            }
            return Err(Error::NotFound);
        }

        // Resolve path through caller's VFS view if provided
        let resolved = resolve_path_for_caller(path, caller_view, "load_elf")?;

        // All other paths go through VFS
        if let Some(data) = self.load_from_vfs(&resolved) {
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
        if self.ensure_vfs_endpoint().is_err() {
            return None;
        }

        let client_id = registry::control_endpoint();
        if client_id == 0 {
            return None;
        }

        let client = VfsClient::new(self.vfs_endpoint, client_id);

        // #region agent log
        let _ = debug_print("procmgr: opening file via VFS...");
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

        if let Some(data) = self.load_from_vfs_ring(&client, file) {
            let _ = client.close(file);
            return Some(data);
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

    fn load_from_vfs_ring(
        &mut self,
        client: &VfsClient,
        file: libcluu::fs::client::VfsFile,
    ) -> Option<Vec<u8>> {
        const RING_BYTES: usize = 64 * 1024;
        let _ = debug_print(&format!(
            "procmgr: load_from_vfs_ring fd={} size={}",
            file.fd, file.size
        ));

        let region = match libcluu::ipc::alloc_shared_ring_region(
            self.space_token,
            RING_BYTES,
            libcluu::ipc::SHARED_RING_DEFAULT_MAP_FLAGS,
        ) {
            Ok(region) => region,
            Err(err) => {
                let _ = debug_print(&format!("procmgr: ring alloc failed {:?}", err));
                return None;
            }
        };

        let ring_meta = match client.setup_read_ring(self.space_token, region.base, region.bytes) {
            Ok(meta) => meta,
            Err(err) => {
                let _ = debug_print(&format!("procmgr: ring setup failed {:?}", err));
                let _ = libcluu::ipc::free_shared_ring_region(self.space_token, region);
                return None;
            }
        };

        if ring_meta.bytes > region.bytes {
            let _ = debug_print("procmgr: ring invalid bytes from vfs");
            let _ = libcluu::ipc::free_shared_ring_region(self.space_token, region);
            return None;
        }

        let backing =
            unsafe { core::slice::from_raw_parts_mut(region.base as *mut u8, ring_meta.bytes) };
        let mut ring = match SharedRing::attach(backing) {
            Ok(ring) => ring,
            Err(err) => {
                let _ = debug_print(&format!("procmgr: ring attach failed {:?}", err));
                let _ = libcluu::ipc::free_shared_ring_region(self.space_token, region);
                return None;
            }
        };

        let mut data = Vec::with_capacity(file.size);
        let mut offset = 0usize;
        let req_chunk = ring_meta.capacity.saturating_sub(1).min(1024 * 1024);

        while offset < file.size {
            let req = (file.size - offset).min(req_chunk);
            if req == 0 {
                break;
            }
            let chunk = match client.read_ring(file, offset, req) {
                Ok(chunk) => chunk,
                Err(err) => {
                    let _ = debug_print(&format!("procmgr: ring read failed {:?}", err));
                    let _ = libcluu::ipc::free_shared_ring_region(self.space_token, region);
                    return None;
                }
            };
            if chunk.len == 0 {
                break;
            }

            let start_len = data.len();
            data.resize(start_len + chunk.len, 0);
            let popped = ring.pop(&mut data[start_len..start_len + chunk.len]);
            if popped != chunk.len {
                let _ = debug_print(&format!(
                    "procmgr: ring pop mismatch expected={} got={}",
                    chunk.len, popped
                ));
                let _ = libcluu::ipc::free_shared_ring_region(self.space_token, region);
                return None;
            }

            offset += popped;
            if chunk.eof {
                break;
            }
        }

        let _ = libcluu::ipc::free_shared_ring_region(self.space_token, region);
        if data.is_empty() {
            return None;
        }
        Some(data)
    }

    fn map_elf_from_vfs(&mut self, path: &str, space_token: usize, caller_view: Option<&ViewMountList>) -> Result<Option<usize>> {
        // Resolve path through caller's VFS view if provided
        let resolved = resolve_path_for_caller(path, caller_view, "map_elf_from_vfs")?;
        let effective_path = resolved.as_str();

        // Ensure we have VFS endpoint
        if self.ensure_vfs_endpoint().is_err() {
            return Ok(None);
        }

        let client_id = registry::control_endpoint();
        if client_id == 0 {
            return Ok(None);
        }

        let client = VfsClient::new(self.vfs_endpoint, client_id);
        let file = match self.cached_vfs_file(&client, effective_path) {
            Ok(file) => file,
            Err(_) => return Ok(None),
        };

        let map_token = token_derive(space_token, Rights::SPACE_MAP.bits() as usize, u64::MAX)?;
        let first_attempt = client.map_elf(file, map_token);
        let entry = match first_attempt {
            Ok(entry) => Some(entry),
            Err(err) => {
                let _ = debug_print(&format!("procmgr: map_elf failed {:?}", err));
                // Likely stale fd or VFS-side eviction: refresh once and retry.
                self.invalidate_cached_vfs_file(&client, effective_path);
                match self.cached_vfs_file(&client, effective_path) {
                    Ok(refreshed_file) => client.map_elf(refreshed_file, map_token).ok(),
                    Err(_) => None,
                }
            }
        };
        Ok(entry)
    }

    fn cached_vfs_file(
        &mut self,
        client: &VfsClient,
        path: &str,
    ) -> Result<libcluu::fs::client::VfsFile> {
        if let Some(file) = self.vfs_file_cache.get(path).copied() {
            return Ok(file);
        }

        let file = client.open(path)?;
        if self.vfs_file_cache.len() >= MAX_VFS_FILE_CACHE_ENTRIES {
            self.evict_one_cached_vfs_file(client);
        }
        self.vfs_file_cache.insert(String::from(path), file);
        Ok(file)
    }

    fn invalidate_cached_vfs_file(&mut self, client: &VfsClient, path: &str) {
        if let Some(stale) = self.vfs_file_cache.remove(path) {
            let _ = client.close(stale);
        }
    }

    fn evict_one_cached_vfs_file(&mut self, client: &VfsClient) {
        let Some(evict_key) = self.vfs_file_cache.keys().next().cloned() else {
            return;
        };
        if let Some(file) = self.vfs_file_cache.remove(&evict_key) {
            let _ = client.close(file);
        }
    }

    fn handle_container_run(
        &mut self,
        msg: &Message,
        payload: &[u8],
        sender_tid: usize,
    ) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 5);

        // Extract FDAC offset and param override info from message words
        let fdac_offset = if msg.tag.words >= 3 { msg.words[2] } else { 0 };
        let param_offset = if msg.tag.words >= 4 { msg.words[3] } else { 0 };
        let param_count = if msg.tag.words >= 5 { msg.words[4] } else { 0 };

        // Extract image name from payload (NUL-terminated, bounded by FDAC or param offset)
        let name_end = if fdac_offset > 0 && fdac_offset <= payload.len() {
            fdac_offset
        } else if param_offset > 0 && param_offset <= payload.len() {
            param_offset
        } else {
            payload.len()
        };
        let image_name = match core::str::from_utf8(&payload[..name_end]) {
            Ok(s) => s.trim_end_matches('\0').trim(),
            Err(_) => {
                let _ = debug_print(&format!(
                    "procmgr: container_run rejected: payload not UTF-8 (len={} name_end={})",
                    payload.len(), name_end
                ));
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Extract FDAC data from payload (after image name)
        let fdac_data = if fdac_offset > 0 && fdac_offset < payload.len() {
            &payload[fdac_offset..]
        } else {
            &[]
        };
        if image_name.is_empty() {
            let _ = debug_print(&format!(
                "procmgr: container_run rejected: empty image name (payload_len={} words={:?})",
                payload.len(), [msg.words[0], msg.words[1], msg.words[2], msg.words[3], msg.words[4]]
            ));
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }
        let _ = debug_print(&format!("procmgr: container run '{}'", image_name));

        // Read manifest.toml from VFS
        let manifest_path = format!("/var/images/{}/manifest.toml", image_name);
        let manifest_contents = match self.read_file_from_vfs(&manifest_path) {
            Some(data) => data,
            None => {
                let _ = debug_print(&format!("procmgr: container manifest not found: {}", manifest_path));
                reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };
        let manifest_str = match core::str::from_utf8(&manifest_contents) {
            Ok(s) => s,
            Err(_) => {
                let _ = debug_print(&format!(
                    "procmgr: container '{}' manifest not valid UTF-8",
                    image_name
                ));
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Parse manifest TOML
        let doc = match libcluu::toml::parse(manifest_str) {
            Ok(d) => d,
            Err(err) => {
                let _ = debug_print(&format!("procmgr: manifest parse error: {}", err));
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Extract binary path
        let binary = match doc.table("exec").and_then(|t| t.get_str("binary")) {
            Some(b) => b,
            None => {
                let _ = debug_print("procmgr: manifest missing [exec] binary");
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Build capability profile from manifest
        let mut requested_profile = CapProfile::USER;
        if let Some(profile_table) = doc.table("profile") {
            if let Some(caps) = profile_table.get_array("capabilities") {
                for cap_name in caps {
                    if let Some(cap) = parse_capability(cap_name) {
                        requested_profile |= cap;
                    } else {
                        let _ = debug_print(&format!(
                            "procmgr: container '{}' unknown capability '{}'",
                            image_name, cap_name
                        ));
                        reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                        if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                        return Ok(());
                    }
                }
            }
        }

        // Validate caller can grant the requested profile
        let caller_profile = if sender_tid == 0 {
            CapProfile::USER
        } else {
            match self.tid_to_pid.get(&sender_tid)
                .and_then(|pid| self.pid_to_profile.get(pid))
                .copied()
            {
                Some(p) => p,
                None => {
                    let _ = debug_print(&format!(
                        "procmgr: container '{}' rejected: sender tid={} not in pid map",
                        image_name, sender_tid
                    ));
                    reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                    if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                    return Ok(());
                }
            }
        };
        if !caller_profile.can_grant(requested_profile) {
            let _ = debug_print(&format!(
                "procmgr: container '{}' profile escalation denied",
                image_name
            ));
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // Fix E: Extract notify_endpoint from caller message (same as handle_spawn_message)
        let notify_endpoint = if msg.tag.words >= 2 { msg.words[1] } else { 0 };
        let notify_endpoint = match self.resolve_notify_endpoint(sender_tid, notify_endpoint) {
            Ok(endpoint) => endpoint,
            Err(err) => {
                let _ = debug_print(&format!(
                    "procmgr: container '{}' notify endpoint resolution failed: {:?}",
                    image_name, err
                ));
                reply_msg.words[0] = err.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Determine parent container and check nesting depth
        let caller_pid = self.tid_to_pid.get(&sender_tid).copied().unwrap_or(0);
        let caller_container_id = self.pid_to_container_id.get(&caller_pid).copied().unwrap_or(0);

        let detach = doc.table("container")
            .and_then(|t| t.get_str("detach"))
            .map(|s| s == "true")
            .unwrap_or(false);
        let parent_cid = if detach { 0 } else { caller_container_id };

        // Check nesting depth limit
        if parent_cid != 0 && self.container_depth(caller_container_id) >= MAX_NESTING_DEPTH {
            let _ = debug_print(&format!(
                "procmgr: container '{}' nesting depth exceeded (max={})",
                image_name, MAX_NESTING_DEPTH
            ));
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // Allocate container_id and create dirs
        let mut container_id = self.next_container_id();
        // Only create ext2 dirs if persistent storage is needed
        let has_persistent_storage = doc
            .table("storage")
            .and_then(|t| t.get_array("persistent_dirs"))
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        let image_dirs: Vec<String> = doc
            .table("storage")
            .and_then(|t| t.get_array("image_dirs"))
            .map(|a| a.iter().map(|s| s.clone()).collect())
            .unwrap_or_default();
        let deny_inherit = doc.table("mounts")
            .and_then(|t| t.get_str("deny_inherit"))
            .map(|s| s == "true")
            .unwrap_or(false);
        let deny_paths: Vec<String> = doc
            .table("mounts")
            .and_then(|t| t.get_array("deny"))
            .map(|a| a.iter().map(|s| s.clone()).collect())
            .unwrap_or_default();
        if has_persistent_storage {
            if !self.create_container_dirs(container_id, image_name) {
                let _ = debug_print(&format!(
                    "procmgr: container '{}' dir creation failed",
                    image_name
                ));
                container_id = 0;
            }
        }
        // else: skip ext2 dir creation — MemFs handles ephemeral storage

        // Resolve the binary path within the image
        let binary_vfs_path = format!("/var/images/{}{}", image_name, binary);

        // PRIORITY: extract from [scheduling] section
        let priority = doc
            .table("scheduling")
            .and_then(|t| t.get_str("priority"))
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_PRIORITY);

        // ENDPOINT: create TOKEN_EXTRA_0 based on [tokens] endpoint_mode
        let endpoint_mode = doc
            .table("tokens")
            .and_then(|t| t.get_str("endpoint_mode"));
        let extra_token = match endpoint_mode {
            Some("listen") => {
                let ep = endpoint_create(self.token)?;
                match token_derive(ep, Rights::IPC_RECV.bits() as usize, u64::MAX) {
                    Ok(t) => t,
                    Err(_) => 0,
                }
            }
            Some("grantable") => {
                let ep = endpoint_create(self.token)?;
                let rights = Rights::IPC_RECV | Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT;
                match token_derive(ep, rights.bits() as usize, u64::MAX) {
                    Ok(t) => t,
                    Err(_) => 0,
                }
            }
            _ => 0,
        };

        // PARAM: read [params] slots from manifest (wire format deferred to vtmgr migration)
        let param_slots: Vec<String> = doc
            .table("params")
            .and_then(|t| t.get_array("slots"))
            .map(|a| a.iter().map(|s| s.clone()).collect())
            .unwrap_or_default();
        if !param_slots.is_empty() {
            let _ = debug_print(&format!(
                "procmgr: container '{}' param slots: {:?}",
                image_name, param_slots
            ));
        }

        // DEVICE: read [hardware] devices from manifest and derive device tokens
        let devices: Vec<String> = doc
            .table("hardware")
            .and_then(|t| t.get_array("devices"))
            .map(|a| a.iter().map(|s| s.clone()).collect())
            .unwrap_or_default();
        let extra_token_1 = if devices.iter().any(|d| d == "irq") {
            let _ = debug_print(&format!(
                "procmgr: container '{}' deriving IRQ token",
                image_name
            ));
            match token_derive(self.token, Rights::IRQ_HANDLE.bits() as usize, u64::MAX) {
                Ok(t) => t,
                Err(_) => 0,
            }
        } else {
            0
        };
        if !devices.is_empty() {
            let _ = debug_print(&format!(
                "procmgr: container '{}' devices: {:?}",
                image_name, devices
            ));
        }

        // Fix B: Build argv payload with binary path as argv[0]
        let mut argv_payload: Vec<u8> = Vec::new();
        argv_payload.extend_from_slice(binary.as_bytes());
        argv_payload.push(0);
        let argc = 1usize;

        // Parse param overrides from payload (G1+G2: wire format extension)
        let mut param_overrides_buf = [(0usize, 0u64); 10];
        let mut n_overrides = 0;
        if param_count > 0 && param_offset > 0 && param_offset < payload.len() {
            let param_data = &payload[param_offset..];
            for i in 0..param_count.min(10) {
                let off = i * 10;
                if off + 10 > param_data.len() { break; }
                let idx = u16::from_le_bytes([param_data[off], param_data[off + 1]]) as usize;
                let val = u64::from_le_bytes(param_data[off + 2..off + 10].try_into().unwrap());
                if idx < 10 {
                    param_overrides_buf[n_overrides] = (idx, val);
                    n_overrides += 1;
                }
            }
        }
        let param_overrides = &param_overrides_buf[..n_overrides];

        // Resolve caller VT and temporarily wire stdout to correct tty
        let caller_vt = self.resolve_caller_vt(sender_tid);
        let tty_ep = if caller_vt < VT_COUNT {
            self.tty_endpoints[caller_vt]
        } else {
            self.tty_endpoints[0]
        };
        let saved_tty = self.tty_endpoints[0];
        if tty_ep != 0 { self.tty_endpoints[0] = tty_ep; }

        // Spawn the process
        let spawn_seq = self.next_spawn_seq();
        let spawn_start = self.clock_sample();

        match self.spawn_service_with_env(
            &binary_vfs_path,
            priority,
            &argv_payload,
            argc,
            &[],
            0,
            sender_tid,
            spawn_seq,
            spawn_start,
            fdac_data,
            requested_profile,
            extra_token,
            extra_token_1,
            param_overrides,
            None, // no caller view (container run uses absolute /var/images/ paths)
        ) {
            Ok((thread_token, cookie, pid, child_stdin_send)) => {
                // Build view: for nested containers, inherit caller's view; for top-level, use default
                let image_dir = format!("/var/images/{}", image_name);
                let mut view_mounts = if caller_container_id != 0 && !deny_inherit {
                    // Nested: start with caller's view as base
                    let caller_view = self.pid_to_view.get(&caller_pid)
                        .cloned()
                        .unwrap_or_else(|| default_view_for_profile(requested_profile));

                    // Filter out denied paths
                    let mut filtered: ViewMountList = caller_view.into_iter()
                        .filter(|(_, dst, _)| {
                            !deny_paths.iter().any(|deny| dst == deny || dst.starts_with(&format!("{}/", deny)))
                        })
                        .collect();

                    // Apply image dir overrides (replace /bin, /lib etc with image versions)
                    apply_image_dir_overrides(&mut filtered, image_name, &image_dirs);
                    filtered
                } else if deny_inherit {
                    // deny_inherit = true: ONLY image dirs + container storage, no passthrough
                    let mut mounts = ViewMountList::new();
                    for dir in &image_dirs {
                        mounts.push((
                            format!("/var/images/{}/{}", image_name, dir),
                            format!("/{}", dir),
                            false, // read-only
                        ));
                    }
                    mounts
                } else {
                    // Top-level: default view (current behavior)
                    let mut mounts = default_view_for_profile(requested_profile);
                    apply_image_dir_overrides(&mut mounts, image_name, &image_dirs);
                    mounts
                };

                // Container-scoped /tmp: replace any passthrough /tmp (first-match-wins ordering)
                if let Some(pos) = view_mounts.iter().position(|(_, dst, _)| dst == "/tmp") {
                    view_mounts.remove(pos);
                }
                if container_id > 0 {
                    view_mounts.insert(0, (
                        format!("/var/containers/c-{}/tmp", container_id),
                        String::from("/tmp"),
                        true,
                    ));
                }

                // Add persistent dirs (container-scoped, writable, first-match-wins)
                if has_persistent_storage && container_id > 0 {
                    if let Some(storage_table) = doc.table("storage") {
                        if let Some(pdirs) = storage_table.get_array("persistent_dirs") {
                            for pdir in pdirs {
                                let dir_name = pdir.trim_start_matches('/');
                                if let Some(pos) = view_mounts.iter().position(|(_, dst, _)| dst.trim_start_matches('/') == dir_name) {
                                    view_mounts.remove(pos);
                                }
                                view_mounts.insert(0, (
                                    format!("/var/containers/c-{}/{}", container_id, dir_name),
                                    format!("/{}", dir_name),
                                    true,
                                ));
                            }
                        }
                    }
                }

                self.pid_to_container_id.insert(pid, container_id);
                self.container_owner_pids.insert(pid);
                self.register_vfs_view_for_thread(thread_token, &view_mounts, requested_profile, container_id);
                self.pid_to_view.insert(pid, view_mounts);
                // Fix C: removed redundant pid_to_profile insert (spawn_service_with_env already does it)

                // Track container instance with parent relationship.
                // For "vt" containers, derive display name from TTY instance
                // param and parent under vtmgr if available.
                let display_name = if image_name == "vt" {
                    let inst_idx = param_overrides.iter()
                        .find(|&&(idx, _)| idx == PARAM_TTY_INSTANCE)
                        .map(|&(_, v)| v)
                        .unwrap_or(0);
                    format!("vt:{}", inst_idx)
                } else {
                    String::from(image_name)
                };
                let effective_parent = if image_name == "vt" && parent_cid == 0 && self.vtmgr_container_id != 0 {
                    self.vtmgr_container_id
                } else {
                    parent_cid
                };
                self.container_instances.insert(container_id, ContainerInstance {
                    name: display_name,
                    container_id,
                    parent_container_id: effective_parent,
                    pid,
                    image_path: image_dir,
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                });

                // Track parent→child for cascading cleanup
                if parent_cid != 0 {
                    self.container_children.entry(parent_cid).or_insert_with(Vec::new).push(container_id);
                }

                // Fix E: Register exit notification so shell can wait
                if sender_tid != 0 {
                    let entry = self.sender_live_children.entry(sender_tid).or_insert(0);
                    *entry = entry.saturating_add(1);
                }
                if notify_endpoint != 0 {
                    self.exit_notify.insert(cookie, notify_endpoint);
                }

                // Fix D: Return cookie in reply for wait() support
                reply_msg.words[0] = 0;
                reply_msg.words[1] = pid;
                reply_msg.words[2] = cookie;
                reply_msg.words[3] = container_id as usize;
                reply_msg.words[4] = child_stdin_send;
                let _ = debug_print(&format!(
                    "procmgr: container '{}' started pid={} cid={}",
                    image_name, pid, container_id
                ));
            }
            Err(err) => {
                let _ = debug_print(&format!(
                    "procmgr: container '{}' spawn failed: {:?}",
                    image_name, err
                ));
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }

        self.tty_endpoints[0] = saved_tty;
        if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
        Ok(())
    }

    fn handle_container_list(&mut self, msg: &Message) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(PROCMGR_CONTAINER_LIST_LABEL, [0; 6], 1);

        let mut listing = String::new();
        for inst in self.container_instances.values() {
            listing.push_str(&format!("{} {} {}\n", inst.name, inst.pid, inst.container_id));
        }

        reply_msg.words[1] = 0; // status
        reply_msg.words[2] = listing.len();
        if let Some(tok) = reply_token {
            if listing.is_empty() {
                let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
            } else {
                let _ = ipc::reply_with_payload(tok, &reply_msg, listing.as_bytes());
            }
        }
        Ok(())
    }

    /// Read a file's contents from VFS into a Vec<u8>.
    fn read_file_from_vfs(&mut self, path: &str) -> Option<Vec<u8>> {
        self.load_from_vfs(path)
    }

    fn handle_kill_message(&mut self, msg: &Message, sender_tid: usize) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(PROCMGR_KILL_LABEL, [0; 6], 1);

        if msg.tag.words < 2 {
            reply_msg.words[0] = (-1isize) as usize; // EINVAL
            if let Some(token) = reply_token {
                let _ = reply(token, &reply_msg, IpcFlags::empty());
            }
            return Ok(());
        }

        let target_pid = msg.words[0];
        let signal = msg.words[1];

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

        let signal_result = match signal {
            SIGSTOP => thread_suspend(thread_token),
            SIGCONT => thread_resume(thread_token),
            SIGINT | SIGTERM | SIGKILL => thread_destroy(thread_token),
            _ => Err(Error::InvalidArgument),
        };

        // Execute signal action and update bookkeeping.
        match signal_result {
            Ok(()) => {
                if signal == SIGINT || signal == SIGTERM || signal == SIGKILL {
                    let child_tid = self.pid_to_tid.get(&target_pid).copied().unwrap_or(0);
                    // Extract container_id before clearing state for cleanup IPC.
                    let container_id = self.pid_to_container_id.remove(&target_pid).unwrap_or(0);
                    self.clear_vfs_view_for_tid(child_tid);
                    self.exit_table.remove(&cookie);
                    self.pid_to_cookie.remove(&target_pid);
                    self.clear_pid_runtime_state(target_pid);
                    self.cookie_to_pid.remove(&cookie);
                    if let Some(owner_tid) = self.pid_owner_tid.remove(&target_pid) {
                        self.on_child_reaped(owner_tid);
                    }
                    // Notify parent so waitpid() unblocks (exit code = 128+signal per POSIX)
                    if let Some(notify_ep) = self.exit_notify.remove(&cookie) {
                        let mut notify_msg = Message::new(PROCMGR_EXIT_LABEL, [0; 6], 2);
                        notify_msg.words[0] = cookie;
                        notify_msg.words[1] = 128 + signal;
                        let _ = send(notify_ep, &notify_msg, IpcFlags::empty());
                    }
                    if let Some(st) = self.cookie_to_space.remove(&cookie) {
                        let _ = space_destroy(st);
                    }
                    // Revoke all derived tokens/endpoints created for this child
                    if let Some(tokens) = self.cookie_to_tokens.remove(&cookie) {
                        for tok in tokens {
                            let _ = token_revoke(tok);
                        }
                    }
                    // Container cleanup only when no other process shares this container.
                    if container_id > 0
                        && !self.pid_to_container_id.values().any(|&cid| cid == container_id)
                    {
                        self.destroy_container_children(container_id);
                        self.container_instances.remove(&container_id);
                        let _ = send_vfs_container_cleanup(self.vfs_endpoint, container_id, 1);
                    }
                }

                reply_msg.words[0] = 0; // Success
                let _ = debug_print(&format!(
                    "procmgr: signal {} pid {} (cookie {})",
                    signal, target_pid, cookie
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

/// Build the default environment variable payload for bootstrap processes.
/// Returns (packed_data, count) where packed_data is "KEY=VALUE\0KEY=VALUE\0...".
fn build_default_env_payload() -> (Vec<u8>, usize) {
    let mut payload = Vec::new();
    for entry in DEFAULT_ENV {
        payload.extend_from_slice(entry.as_bytes());
        payload.push(0);
    }
    (payload, DEFAULT_ENV.len())
}

fn build_user_env_payload(username: &str, home: &str) -> (Vec<u8>, usize) {
    let entries: [alloc::string::String; 5] = [
        format!("PATH=/bin"),
        format!("HOME={}", home),
        format!("SHELL=/bin/shell"),
        format!("USER={}", username),
        format!("TERM=cluu"),
    ];
    let mut payload = Vec::new();
    for entry in &entries {
        payload.extend_from_slice(entry.as_bytes());
        payload.push(0);
    }
    (payload, entries.len())
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
/// Param index for environment variable count.
const PARAM_ENVC: usize = 8;
/// Param index for the byte offset within the info page where env data starts.
const PARAM_ENV_OFFSET: usize = 9;

/// Default environment variables for the initial shell process.
const DEFAULT_ENV: &[&str] = &[
    "PATH=/bin",
    "HOME=/",
    "SHELL=/bin/shell",
    "USER=root",
    "TERM=cluu",
];

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
    env_data: &[u8],
    envc: usize,
    pipe_mask: u8,
    _profile: CapProfile,
    extra_token: usize,
    extra_token_1: usize,
    param_overrides: &[(usize, u64)],
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
    // Slots 9-15: Contextual
    if extra_token != 0 {
        tokens[TOKEN_EXTRA_0] = extra_token;
    }
    if extra_token_1 != 0 {
        tokens[TOKEN_EXTRA_1] = extra_token_1;
    }

    let mut params = [0u64; 10];
    // params[0] = pipe_mask for regular processes (shared with PARAM_FB_BASE for console)
    params[0] = pipe_mask as u64;
    // NOTE: PARAM_CAP_PROFILE (slot 5) is NOT written here. The cap profile is
    // tracked server-side in pid_to_profile. Slot 5 is shared with
    // PARAM_CONSOLE_INSTANCE for console, so writing the profile here would
    // corrupt the instance ID. Callers use param_overrides for slot 5 if needed.
    // Apply caller-specified param overrides (e.g. instance IDs, FB params).
    for &(idx, val) in param_overrides {
        if idx < params.len() {
            params[idx] = val;
        }
    }

    let info_offset = PROCESS_INFO_ADDR - page_base;
    let info_size = size_of::<ProcessInfo>();
    let argv_data_offset = info_offset + info_size; // byte offset within page

    // Compute data offsets and validate bounds before setting params.
    // argv and env data are packed after the ProcessInfo struct in the same page.
    // If either would overflow the page, its params are left at 0 (child sees no data).
    let env_data_offset = argv_data_offset + argv_payload.len();
    let argv_end = argv_data_offset + argv_payload.len();
    let env_end = env_data_offset + env_data.len();

    let argv_fits = argc > 0 && !argv_payload.is_empty() && argv_end <= PAGE_SIZE;
    let env_fits = envc > 0 && !env_data.is_empty() && env_end <= PAGE_SIZE;

    // Only set params if the data actually fits — prevents child from reading garbage
    if argv_fits {
        params[PARAM_ARGC] = argc as u64;
        params[PARAM_ARGV_OFFSET] = argv_data_offset as u64;
    }
    if env_fits {
        params[PARAM_ENVC] = envc as u64;
        params[PARAM_ENV_OFFSET] = env_data_offset as u64;
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
    if argv_fits {
        page[argv_data_offset..argv_end].copy_from_slice(argv_payload);
    }

    // Write env data after argv data (packed "KEY=VALUE\0" strings)
    if env_fits {
        page[env_data_offset..env_end].copy_from_slice(env_data);
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

/// Map a CapProfile to the kernel Rights for each of the 16 token slots.
///
/// This is the policy bridge between the abstract profile bitmask and
/// concrete token derivations. TOKEN_CLOCK is handled separately (always
/// wired unconditionally). TOKEN_EXTRA slots (9-15) are populated by
/// service-specific logic, not the profile.
fn profile_to_rights(profile: CapProfile) -> [Rights; 16] {
    let mut r = [Rights::empty(); 16];

    // Stdio: always present regardless of profile.
    // Even SANDBOXED processes get pre-wired stdio tokens.
    r[TOKEN_STDIN] = Rights::IPC_SEND | Rights::IPC_RECV;
    r[TOKEN_STDOUT] = Rights::IPC_SEND;
    r[TOKEN_STDERR] = Rights::IPC_SEND;
    r[TOKEN_STDLOG] = Rights::IPC_SEND;

    // THREAD_CONTROL on TOKEN_SPACE is always needed for TLS initialization
    // (init_tls → thread_set_fs_base). Without this, FS base stays 0 and any
    // __thread / TLS access causes PAGE_FAULT.
    r[TOKEN_SPACE] |= Rights::THREAD_CONTROL;

    // IPC capability: basic send/recv for endpoint communication.
    if profile.contains(CapProfile::IPC) {
        r[TOKEN_IPC] |= Rights::IPC_SEND | Rights::IPC_RECV;
    }

    // SPAWN, REGISTRY, or VFS: need CREATE+GRANT on IPC cap to create endpoints.
    // VFS requires registry lookup (endpoint_create) for service discovery.
    if profile.contains(CapProfile::SPAWN) || profile.contains(CapProfile::REGISTRY) || profile.contains(CapProfile::VFS) {
        r[TOKEN_IPC] |= Rights::CREATE | Rights::GRANT;
    }

    // SPAWN, REGISTRY, or VFS: need IPC_CALL for synchronous request/reply.
    if profile.contains(CapProfile::SPAWN)
        || profile.contains(CapProfile::REGISTRY)
        || profile.contains(CapProfile::VFS)
    {
        r[TOKEN_IPC] |= Rights::IPC_CALL;
    }

    // SPAWN: needs CREATE+GRANT on self token to create child threads.
    if profile.contains(CapProfile::SPAWN) {
        r[TOKEN_SELF] |= Rights::CREATE | Rights::GRANT;
    }

    // VFS: needs SPACE_MAP to map file data into address space.
    if profile.contains(CapProfile::VFS) {
        r[TOKEN_SPACE] |= Rights::SPACE_MAP;
    }

    // DEVICE: needs THREAD_CONTROL for interrupt handling threads.
    if profile.contains(CapProfile::DEVICE) {
        r[TOKEN_SELF] |= Rights::THREAD_CONTROL;
    }

    // SPACE_GRANT: needs SPACE_GRANT+CREATE on space for shared memory.
    if profile.contains(CapProfile::SPACE_GRANT) {
        r[TOKEN_SPACE] |= Rights::SPACE_GRANT | Rights::CREATE;
    }

    r
}

/// Derive a child token with the given rights, or return 0 if rights are empty.
fn derive_slot(base: usize, rights: Rights) -> Result<usize> {
    if rights.is_empty() {
        Ok(0)
    } else {
        token_derive(base, rights.bits() as usize, u64::MAX)
    }
}

/// Resolve a virtual path through a VFS view's mount table.
/// Returns the concrete filesystem path, or None if not visible in the view.
fn resolve_path_in_view(path: &str, view: &[(String, String, bool)]) -> Option<String> {
    // Must be absolute
    if !path.starts_with('/') {
        return None;
    }
    // Reject ".." and "." components (prevent mount escape)
    for component in path.split('/') {
        if component == ".." || component == "." {
            return None;
        }
    }

    for (src, dst, _writable) in view {
        // Root mount matches everything
        if dst == "/" {
            return Some(format!("{}{}", src, path));
        }
        // Exact match: path == dst
        if path == dst {
            return Some(src.clone());
        }
        // Prefix match: path starts with dst + "/"
        if let Some(suffix) = path.strip_prefix(dst.as_str()) {
            if suffix.starts_with('/') {
                return Some(format!("{}{}", src, suffix));
            }
        }
    }
    None
}

/// Resolve a path through an optional caller VFS view, with debug logging on failure.
fn resolve_path_for_caller(
    path: &str,
    caller_view: Option<&ViewMountList>,
    context: &str,
) -> Result<String> {
    if let Some(view) = caller_view {
        match resolve_path_in_view(path, view) {
            Some(p) => Ok(p),
            None => {
                let _ = debug_print(&format!(
                    "procmgr: {} path '{}' not visible in caller view", context, path
                ));
                Err(Error::NotFound)
            }
        }
    } else {
        Ok(String::from(path))
    }
}

/// Generate the default VFS view mounts for a capability profile.
///
/// Returns a list of (src, dst, writable) tuples.  For Phase C these are
/// identity mappings (src == dst); Phase D will add path remapping.
fn default_view_for_profile(profile: CapProfile) -> ViewMountList {
    libcluu::vfs_view::default_mounts_for_profile(profile)
        .iter()
        .map(|&(src, dst, w)| (String::from(src), String::from(dst), w))
        .collect()
}

/// Check whether `child_view` is a valid narrowing of `parent_view`.
///
/// Every mount in the child view must be covered by a mount in the parent view
/// with an equal or wider prefix and equal or greater write permission.
/// An empty parent view means no filesystem access — no child mount can pass.
/// An empty child view is always valid (child requests no access).
fn can_narrow_view(parent_view: &[(String, String, bool)], child_view: &[(String, String, bool)]) -> bool {
    for (child_src, child_dst, child_writable) in child_view {
        let covered = parent_view.iter().any(|(p_src, p_dst, p_writable)| {
            // Child prefix must be under (or equal to) parent prefix.
            let src_ok = child_src == p_src
                || (child_src.starts_with(p_src.as_str())
                    && child_src.as_bytes().get(p_src.len()) == Some(&b'/'))
                || p_src == "/";
            let dst_ok = child_dst == p_dst
                || (child_dst.starts_with(p_dst.as_str())
                    && child_dst.as_bytes().get(p_dst.len()) == Some(&b'/'))
                || p_dst == "/";
            // Child can't request write if parent is read-only.
            let write_ok = !child_writable || *p_writable;
            src_ok && dst_ok && write_ok
        });
        if !covered {
            return false;
        }
    }
    true
}

/// Override default view mounts with per-image directory paths.
///
/// For each `image_dir` (e.g. "bin", "lib"), if there's a matching read-only mount
/// with dst == "/<dir>", redirect its src to `/var/images/<image_name>/<dir>` so the
/// container sees its own binaries instead of the global ones.
fn apply_image_dir_overrides(mounts: &mut ViewMountList, image_name: &str, image_dirs: &[String]) {
    for mount in mounts.iter_mut() {
        for dir in image_dirs {
            let virtual_path = format!("/{}", dir);
            if mount.1 == virtual_path && !mount.2 {
                // Override src to point to image-specific directory
                mount.0 = format!("/var/images/{}/{}", image_name, dir);
            }
        }
    }
}

/// Serialize and send a VFS_SET_VIEW message for a newly spawned child.
///
/// `client_tid` is the child's kernel `ThreadId` (the `sender_tid` seen by VFS).
/// `mounts` is the list of (src, dst, writable) tuples from `default_view_for_profile`.
/// A `client_tid` of 0 means "sender_tid" (manager self-view bootstrap path).
fn send_vfs_set_view(
    vfs_endpoint: usize,
    client_tid: usize,
    mounts: &[(String, String, bool)],
    profile: CapProfile,
    container_id: u64,
) -> Result<()> {
    if vfs_endpoint == 0 {
        return Ok(());
    }

    // Serialize payload: per mount: u16 src_len + u16 dst_len + u8 flags + src + dst
    let mut payload = Vec::new();
    for (src, dst, writable) in mounts {
        let src_bytes = src.as_bytes();
        let dst_bytes = dst.as_bytes();
        payload.extend_from_slice(&(src_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(&(dst_bytes.len() as u16).to_le_bytes());
        payload.push(if *writable { 1u8 } else { 0u8 });
        payload.extend_from_slice(src_bytes);
        payload.extend_from_slice(dst_bytes);
    }

    let mut msg = Message::new(ipc::VFS_SET_VIEW_LABEL, [0; 6], 5);
    msg.words[0] = payload.len();
    msg.words[1] = client_tid;
    msg.words[2] = mounts.len();
    msg.words[3] = profile.bits() as usize;
    // container_id is u64; usize is 64-bit on x86_64 so the cast is lossless.
    msg.words[4] = container_id as usize;
    ipc::send_msg_with_payload(vfs_endpoint, &msg, &payload)
}

/// Send a VFS_CONTAINER_CLEANUP message to VFS for container storage cleanup.
///
/// `mode` 0 = exit (delete tmp/ contents only), 1 = destroy (delete entire container tree).
fn send_vfs_container_cleanup(vfs_endpoint: usize, container_id: u64, mode: usize) -> Result<()> {
    if vfs_endpoint == 0 || container_id == 0 {
        return Ok(());
    }
    let mut msg = Message::new(ipc::VFS_CONTAINER_CLEANUP_LABEL, [0; 6], 3);
    msg.words[0] = 0; // no payload
    msg.words[1] = container_id as usize;
    msg.words[2] = mode;
    send(vfs_endpoint, &msg, IpcFlags::empty())
}

/// Map a capability name string to the corresponding CapProfile flag.
fn parse_capability(name: &str) -> Option<CapProfile> {
    match name {
        "ipc" => Some(CapProfile::IPC),
        "spawn" => Some(CapProfile::SPAWN),
        "registry" => Some(CapProfile::REGISTRY),
        "vfs" => Some(CapProfile::VFS),
        "device" => Some(CapProfile::DEVICE),
        "space_grant" => Some(CapProfile::SPACE_GRANT),
        "net" => Some(CapProfile::NET),
        "admin" => Some(CapProfile::ADMIN),
        _ => None,
    }
}

fn parse_profile_str(s: &str) -> Option<CapProfile> {
    match s {
        "sandboxed" => Some(CapProfile::SANDBOXED),
        "user" => Some(CapProfile::USER),
        "service" => Some(CapProfile::SERVICE),
        "admin" => Some(CapProfile::ADMIN_PROFILE),
        "supervisor" => Some(CapProfile::SUPERVISOR),
        _ => None,
    }
}

