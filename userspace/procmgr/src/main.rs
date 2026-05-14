#![no_std]
#![no_main]

extern crate alloc;

mod envelopes;
mod mount_policy;
mod pg_table;

use crate::mount_policy::{
    parse_mount_policies_raw, resolve_effective_policies, validate_cluufile_against_parent,
    MountPolicy, MountPolicyEntry,
};
use alloc::{collections::BTreeMap, collections::BTreeSet, format, string::String, vec::Vec};
use core::mem::{size_of, take};
use libcluu::boot::{
    process_info,
    ProcessInfo,
    PARAM_CONSOLE_ACTIVE,
    PARAM_CONSOLE_INSTANCE,
    CWD_MAX,
    PARAM_CWD_LEN,
    PARAM_CWD_OFFSET,
    PARAM_REDIR_LEN,
    PARAM_REDIR_OFFSET,
    PARAM_FD_VFS_OFFSET,
    PARAM_FD_VFS_LEN,
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
    TOKEN_SELF_THREAD,
    TOKEN_SPACE,
    TOKEN_STDERR,
    TOKEN_STDIN,
    TOKEN_STDLOG,
    TOKEN_STDOUT,
};
use libcluu::cap::CapProfile;
use libcluu::crypto;
use libcluu::elf::ElfFile;
use libcluu::fs::client::VfsClient;
use libcluu::ipc::extract_reply_id;
use libcluu::ipc::parse_message;
use libcluu::ipc::SharedRing;
use libcluu::ipc::CWD_MAGIC as SPAWN_CWD_MAGIC;
use libcluu::ipc::ENV_MAGIC as SPAWN_ENV_MAGIC;
use libcluu::ipc::REDIR_MAGIC as SPAWN_REDIR_MAGIC;
use libcluu::ipc::PROCMGR_CONTAINER_LIST_LABEL;
use libcluu::ipc::PROCMGR_CONTAINER_RUN_LABEL;
use libcluu::ipc::PROCMGR_CONTAINER_STATS_LABEL;
use libcluu::ipc::PROCMGR_JOB_NOTIFY_LABEL;
use libcluu::ipc::PROCMGR_PG_ATTACH_LABEL;
use libcluu::ipc::PROCMGR_PG_CREATE_LABEL;
use libcluu::ipc::PROCMGR_PG_RESUME_LABEL;
use libcluu::ipc::PROCMGR_PG_SIGNAL_LABEL;
use libcluu::ipc::PROCMGR_PG_SUSPEND_LABEL;
use libcluu::ipc::PROCMGR_PID_PGID_QUERY_LABEL;
use libcluu::ipc::PROCMGR_PROC_QUERY_LABEL;
use libcluu::ipc::PROCMGR_QUERY_CTTY_LABEL;
use libcluu::ipc::PROCMGR_SPAWN_SERVICE_LABEL;
use crate::pg_table::PgTable;
use libcluu::registry;
use libcluu::syscall::{
    space_destroy, thread_destroy, thread_get_id, thread_resume, thread_set_fault_endpoint,
    thread_suspend, token_revoke, THREAD_CREATE_START_SUSPENDED,
};
use libcluu::tar::find_member;
use libcluu::*;

/// Per-mount entry sent to VFS: (src, dst, writable, memfs_cid).
/// `memfs_cid = 0` → mount resolves against the global MountTable (filesystem-backed).
/// `memfs_cid > 0` → mount resolves against that container's MemFs backend.
type ViewMountList = Vec<(String, String, bool, u64)>;

#[derive(Clone, Debug)]
enum RestartPolicy {
    Never,
    Always,
    OnFailure { max_restarts: usize, window_secs: u64 },
}

const DEFAULT_MAX_RESTARTS_ON_FAILURE: usize = 3;
const DEFAULT_RESTART_WINDOW_SECS: u64 = 300;

/// Safety valve: Always-restart containers are rate-limited when they exceed
/// this many restarts within the safety window.
const ALWAYS_SAFETY_VALVE_COUNT: usize = 10;
const ALWAYS_SAFETY_VALVE_WINDOW_SECS: u64 = 60;
/// Exponential backoff parameters for OnFailure restarts.
const ONFAILURE_BACKOFF_BASE_SECS: u64 = 1;
const ONFAILURE_BACKOFF_CAP_SECS: u64 = 30;

/// Action to perform when a timer expires.
enum TimerAction {
    Restart(u64), // container_id
}

/// A pending timer entry in the timer queue.
struct TimerEntry {
    deadline: u64, // TSC tick deadline
    action: TimerAction,
}

#[derive(Clone, Debug, Default)]
struct QuotaSpec {
    max_processes: Option<u32>,
    max_priority: Option<u8>,
}

struct ContainerInstance {
    name: String,
    instance_name: String,      // "editor", "editor.2", etc.
    session_id: u64,            // 0 = system/autostart
    container_id: u64,
    parent_container_id: u64, // 0 = top-level or detached
    pid: usize,
    image_path: String,
    #[allow(dead_code)]
    mapped_pages: u32,
    restart_policy: RestartPolicy,
    restart_count: usize,
    last_exit_code: i32,
    restart_attempt_start: u64,
    quota: QuotaSpec,
    live_processes: u32,
}

struct PendingVfsView {
    client_tid: usize,
    mounts: ViewMountList,
    profile: CapProfile,
    container_id: u64,
}

struct UserRecord {
    home: String,
    #[allow(dead_code)]
    shell: String,
    profile: CapProfile,
    profile_name: String,
    escalate: Option<CapProfile>,
    password: String,
}

struct LoginAttempt {
    fail_count: u32,
    last_fail_tick: u64,
}

struct SessionEntry {
    container_id: u64,
    shell_cid: u64,
    #[allow(dead_code)]
    pid: usize,
    username: String,
    profile: CapProfile,
    vt_index: usize,
    stdin_endpoint: usize,
}

/// One pipe — an IPC endpoint with two rights-restricted tokens minted from it.
/// See docs/superpowers/specs/2026-04-27-pipes-design.md §4.
struct PipeEntry {
    /// Underlying endpoint root token owned by procmgr.
    endpoint: usize,
    /// PID of the process that called PIPE_CREATE — used for cleanup on exit.
    creator_pid: usize,
    /// Send-only token derived from `endpoint`. Cleared once revoked.
    write_token: usize,
    /// Recv-only token derived from `endpoint`. Cleared once revoked.
    read_token: usize,
}

/// Upper bound on the argv block emitted by `build_container_run_payload_with_argv`.
/// The child's argv must fit inside the 4 KB ProcessInfo page alongside the
/// ProcessInfo header, cwd block, and name; 3 KB leaves ~1 KB for the rest.
/// `libcluu::args` enforces the corresponding read-side cap via its
/// `argv_offset >= PAGE_SIZE` guard.
const MAX_ARGV_TRAILER_BYTES: usize = 3072;

const SERVICE_STACK_SIZE: usize = 64 * 1024;
const SERVICE_STACK_BASE: usize = 0x6d000000;
const SERVICE_STACK_TOP: usize = SERVICE_STACK_BASE + SERVICE_STACK_SIZE;
const STACK_FLAGS: usize = 0x03; // read + write
                                 // PAGE_SIZE is imported from libcluu::*
const SERVICE_PATH: &str = "/var/images/vt/bin/shell";
use libcluu::build_env::SHELL_AUTOSTART_CMD;
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
    /// Cached value of procmgr's main-thread tid; 0 means not yet looked up.
    /// Used by VFS-backed FDAC injection (see `procmgr_main_tid`).
    cached_main_tid: usize,
    /// virtio-blk listen endpoint for BLK_TID_CLEANUP broadcasts. Resolved
    /// lazily on first cleanup so we don't depend on blkdev boot order.
    blkdev_endpoint: usize,
    space_token: usize,     // Our address space token for grants
    grant_base_next: usize, // Reused base address for grant buffer
    clock_token: usize,
    clock_freq: u64,
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
    envelopes: Vec<envelopes::Envelope>,
    user_records: BTreeMap<String, UserRecord>,
    session_table: BTreeMap<u64, SessionEntry>,
    vt_to_session: [u64; VT_COUNT],
    /// Container ID of the vtmgr service (set during autostart).
    vtmgr_container_id: u64,
    shutting_down: bool,
    shutdown_action: u8, // 0=poweroff, 1=reboot
    autostart_order: Vec<u64>,
    /// Per-(session, image_name) monotonic counter for instance naming.
    instance_counters: BTreeMap<(u64, String), u32>,
    /// Timer queue for deferred actions (exponential backoff restarts).
    pending_timers: Vec<TimerEntry>,
    /// Container IDs that have a pending deferred restart (prevents duplicates).
    pending_restarts: BTreeSet<u64>,
    /// Persistent shared-ring region for VFS bulk reads. Allocated once on
    /// first `load_from_vfs_ring` and reused for every subsequent call so
    /// the VFS-side grant survives across loads.
    vfs_read_ring: Option<libcluu::ipc::SharedRingRegion>,
    /// Per-user failed login attempt tracking for rate limiting.
    login_attempts: BTreeMap<String, LoginAttempt>,
    /// Pipe table: index = lower 16 bits of pipe_id; `None` means free slot.
    pipes: Vec<Option<PipeEntry>>,
    /// Process-group table: pgid → [pid] (Phase 4 Plan D job control).
    pg_table: PgTable,
    /// PIDs currently in the Stopped state (suspended via job control).
    pid_stopped: BTreeSet<usize>,
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
            vfs_read_ring: None,
            vfs_endpoint: 0,
            cached_main_tid: 0,
            blkdev_endpoint: 0,
            space_token: info.tokens[TOKEN_SPACE],
            grant_base_next: 0x50100000, // Start after virtqueue region
            clock_token: info.tokens[TOKEN_CLOCK],
            clock_freq: clock_frequency(info.tokens[TOKEN_CLOCK]).unwrap_or(1_000_000_000),
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
            envelopes: Vec::new(),
            user_records: BTreeMap::new(),
            session_table: BTreeMap::new(),
            vt_to_session: [0; VT_COUNT],
            vtmgr_container_id: 0,
            shutting_down: false,
            shutdown_action: 0,
            autostart_order: Vec::new(),
            instance_counters: BTreeMap::new(),
            pending_timers: Vec::new(),
            pending_restarts: BTreeSet::new(),
            login_attempts: BTreeMap::new(),
            pipes: Vec::new(),
            pg_table: PgTable::new(),
            pid_stopped: BTreeSet::new(),
        })
    }

    /// Allocate a free slot in the pipe table; returns its index.
    /// Encodes to a `pipe_id` via `pipe_id_encode`.
    fn allocate_pipe_slot(&mut self) -> usize {
        for (idx, slot) in self.pipes.iter().enumerate() {
            if slot.is_none() {
                return idx;
            }
        }
        self.pipes.push(None);
        self.pipes.len() - 1
    }

    /// Encode a pipe table index into the `pipe_id` value returned to callers.
    /// v1 keeps the encoding trivial (index in low 16 bits, generation reserved
    /// in upper bits but unused); future generations counter for ABA-safe
    /// reuse can plug in here without an API change.
    fn pipe_id_encode(index: usize) -> usize {
        index & 0xFFFF
    }

    /// Decode a `pipe_id` into a slot index.
    fn pipe_id_decode(pipe_id: usize) -> usize {
        pipe_id & 0xFFFF
    }

    fn clock_sample(&self) -> u64 {
        if self.clock_token == 0 {
            return 0;
        }
        clock_now(self.clock_token).unwrap_or(0)
    }

    fn audit_log(&self, severity: &str, event: &str, details: &str) {
        let ticks = self.clock_sample();
        let line = format!("[{}] AUDIT {} {}: {}", ticks, severity, event, details);
        let _ = debug_print(&line);
    }

    /// Check if a user is rate-limited. Returns true if the backoff period has not expired.
    fn is_rate_limited(&self, username: &str) -> bool {
        let attempt = match self.login_attempts.get(username) {
            Some(a) if a.fail_count > 0 => a,
            _ => return false,
        };
        let now = self.clock_sample();
        // delay = min(1s * 2^(fails-1), 5min) in ticks
        let exp = (attempt.fail_count - 1).min(8); // cap shift to avoid overflow
        let delay_secs: u64 = (1u64 << exp).min(300);
        let delay_ticks = delay_secs * self.clock_freq;
        now.saturating_sub(attempt.last_fail_tick) < delay_ticks
    }

    /// Record a failed authentication attempt for a user.
    fn record_auth_failure(&mut self, username: &str) {
        let now = self.clock_sample();
        let attempt = self.login_attempts
            .entry(String::from(username))
            .or_insert(LoginAttempt { fail_count: 0, last_fail_tick: 0 });
        attempt.fail_count = attempt.fail_count.saturating_add(1);
        attempt.last_fail_tick = now;
    }

    /// Clear failed attempt tracking for a user on successful authentication.
    fn clear_auth_failures(&mut self, username: &str) {
        self.login_attempts.remove(username);
    }

    fn next_spawn_seq(&mut self) -> usize {
        let seq = self.spawn_seq_next;
        self.spawn_seq_next = self.spawn_seq_next.wrapping_add(1);
        seq
    }

    /// Generate a human-friendly instance name for a container.
    /// First instance of "editor" in session 5 → "editor"
    /// Second → "editor.2", third → "editor.3", etc.
    fn next_instance_name(&mut self, session_id: u64, image_name: &str) -> String {
        let key = (session_id, String::from(image_name));
        let counter = self.instance_counters.entry(key).or_insert(0);
        *counter += 1;
        if *counter == 1 {
            String::from(image_name)
        } else {
            format!("{}.{}", image_name, counter)
        }
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
        // Only the plain ADMIN_PROFILE (USER | ADMIN) uses the restricted admin
        // session mounts. Supervisor/service profiles contain ADMIN bits too but
        // need the full device- and root-aware mount set.
        let base_mounts = if profile == CapProfile::ADMIN_PROFILE {
            libcluu::vfs_view::admin_session_mounts()
        } else {
            libcluu::vfs_view::default_mounts_for_profile(profile)
        };
        let mut mounts: ViewMountList = base_mounts.iter()
            .filter(|&&(_, dst, _)| !dst.starts_with("/home/"))
            .map(|&(src, dst, w)| (String::from(src), String::from(dst), w, 0u64))
            .collect();
        mounts.push((String::from(home), String::from(home), true, 0u64));
        mounts
    }

    /// Build a VFS view directly from a resolved envelope's mount list.
    /// Each MountSpec becomes a 1:1 path binding (src == dst); writability is
    /// derived from MountMode. memfs_cid=0 means "resolve against the global
    /// MountTable" — per-Cluufile MOUNT private uses non-zero memfs_cid and
    /// is layered on later (Phase 4).
    fn build_view_from_envelope(envelope: &crate::envelopes::Envelope) -> ViewMountList {
        envelope.mounts.iter().map(|m| {
            let writable = matches!(m.mode, crate::envelopes::MountMode::Rw);
            (m.path.clone(), m.path.clone(), writable, 0u64)
        }).collect()
    }

    /// Build a ViewMountList from already-resolved mount strings.
    /// Each entry is "mode:path". Unknown modes or malformed entries are skipped.
    fn build_view_from_mount_strings(strings: &[String]) -> ViewMountList {
        let mut mounts = ViewMountList::new();
        for raw in strings {
            let (mode_str, path) = match raw.split_once(':') {
                Some(p) => p,
                None => continue,
            };
            let writable = match mode_str {
                "ro" | "readonly"  => false,
                "rw" | "readwrite" => true,
                _ => continue,
            };
            mounts.push((String::from(path), String::from(path), writable, 0u64));
        }
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

    fn log_spawn_stage(&self, _seq: usize, _stage: &str, _start_ts: u64) {
        // Per-stage spawn timing was useful when diagnosing slow boot; in
        // normal operation it produces 6 lines per spawn at INFO.  Re-enable
        // by un-stubbing if you need the breakdown again.
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

    /// Open `/dev/tty<vt>` via procmgr's own VFS client. Used at session-login
    /// to seed FDAC entries for the legacy text shell.
    ///
    /// Returns `(client_id, remote_fd)` — the pair that the FDAC parser
    /// expects on a VFS-backed FdAction. `client_id` is procmgr's main-thread
    /// tid (what VFS authenticated the open under); `remote_fd` is VFS's
    /// table fd for the open. Both nonzero on success.
    fn open_dev_tty_for_session(&mut self, vt: usize) -> Result<(usize, usize)> {
        if vt >= 4 {
            return Err(Error::InvalidArgument);
        }
        if self.vfs_endpoint == 0 {
            self.ensure_vfs_endpoint()?;
        }
        // VFS authenticates by kernel sender_tid; pass 0 as the client-id
        // hint, VFS will overwrite with procmgr's authenticated tid.
        let client = VfsClient::new(self.vfs_endpoint, 0);
        let path = match vt {
            0 => "/dev/tty0",
            1 => "/dev/tty1",
            2 => "/dev/tty2",
            3 => "/dev/tty3",
            _ => unreachable!(),
        };
        const O_RDWR: usize = 2;
        let file = client.open_with(path, O_RDWR, 0)?;
        // Procmgr's main-thread tid is the sender_tid the kernel uses for
        // every IPC from procmgr's main loop. Capture once and cache.
        let procmgr_tid = self.procmgr_main_tid()?;
        Ok((procmgr_tid, file.fd))
    }

    /// Return procmgr's main-thread tid, caching after first lookup.
    ///
    /// init populates TOKEN_SELF_THREAD with a Thread-typed capability for
    /// procmgr's main thread before resuming it.  We call `thread_get_id` on
    /// that capability to obtain the tid that VFS keys client_id entries under.
    fn procmgr_main_tid(&mut self) -> Result<usize> {
        if self.cached_main_tid != 0 {
            return Ok(self.cached_main_tid);
        }
        let info = libcluu::boot::process_info();
        let thread_token = info.tokens[TOKEN_SELF_THREAD];
        if thread_token == 0 {
            return Err(Error::InvalidState);
        }
        let tid = thread_get_id(thread_token)?;
        self.cached_main_tid = tid;
        Ok(tid)
    }

    /// Build an FDAC payload that targets fd 0/1/2 at the same VFS-backed
    /// file. Used by the legacy text-VT session spawn.
    ///
    /// Layout (matches libcluu/src/posix/process.rs):
    ///   u32 magic = 0x46444143
    ///   u32 count = 3
    ///   3 × FdAction { u32 target_fd, u32 flags, usize endpoint,
    ///                  usize vfs_client_id, usize vfs_remote_fd }
    fn build_devtty_fdac(&self, vfs_client_id: usize, vfs_remote_fd: usize)
        -> Vec<u8>
    {
        const FDAC_MAGIC: u32 = 0x46444143;
        let mut out = Vec::with_capacity(8 + 3 * 32);
        out.extend_from_slice(&FDAC_MAGIC.to_le_bytes());
        out.extend_from_slice(&3u32.to_le_bytes());
        for target_fd in 0u32..=2u32 {
            out.extend_from_slice(&target_fd.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());           // flags
            out.extend_from_slice(&0usize.to_le_bytes());         // endpoint (unused for VFS-backed)
            out.extend_from_slice(&vfs_client_id.to_le_bytes());
            out.extend_from_slice(&vfs_remote_fd.to_le_bytes());
        }
        out
    }

    /// Install a VFS view for a thread, then resume it.
    ///
    /// Used in conjunction with `THREAD_CREATE_START_SUSPENDED` to close the
    /// race where a freshly-spawned thread could make VFS calls before its
    /// view was installed. Caller passes a thread that was created suspended;
    /// this helper sends VFS_SET_VIEW (or queues it if VFS isn't up yet),
    /// then resumes the thread.
    ///
    /// On thread_resume failure, logs and best-effort destroys the thread —
    /// otherwise we'd leak a forever-suspended thread.
    ///
    /// # Monotone-view assertion (debug builds only)
    ///
    /// In debug builds this function asserts that `mounts` does not widen the
    /// parent container's recorded view.
    ///
    /// **Callers must pass the correct `parent_container_id` and
    /// `is_identity_switch`**:
    /// - Pass `parent_container_id = 0` (and `is_identity_switch = false`) for
    ///   top-level spawns where procmgr is the authoritative root.
    /// - Pass the actual parent container ID (and `is_identity_switch = false`)
    ///   for nested containers (POSIX spawn, CONTAINER_RUN without detach).
    /// - Pass the caller's container ID (and `is_identity_switch = true`) for
    ///   authenticated identity switches (escalate/sudo, su).  The assertion is
    ///   explicitly skipped for these paths because the target view is
    ///   authorized by procmgr policy, not constrained by the caller's view.
    ///
    /// **Why the parameter, not a registry lookup**: this function is called
    /// *before* the child's `container_instances` entry is inserted, so
    /// `container_instances[container_id]` does not exist yet.  Looking up the
    /// parent through the child would always return `None`, making the
    /// assertion permanently dead.
    ///
    /// **Exemptions** (assertion skipped):
    /// - `parent_container_id == 0` — top-level service; procmgr's SUPERVISOR
    ///   view is always the root.  No parent to check against.
    /// - `is_identity_switch == true` — escalate/sudo or su: caller's view is
    ///   irrelevant; the new view is determined by the *target user's*
    ///   pre-authorized envelope.  Procmgr is the authoritative spawner.
    fn install_view_and_run(
        &mut self,
        thread_token: usize,
        mounts: &[(String, String, bool, u64)],
        profile: CapProfile,
        container_id: u64,
        parent_container_id: u64,
        is_identity_switch: bool,
    ) {
        // ── monotone-view debug assertion ─────────────────────────────────────
        #[cfg(debug_assertions)]
        if parent_container_id != 0 && !is_identity_switch {
            // Look up a pid that owns the parent container to get its recorded view.
            let parent_view_opt = self.container_instances
                .get(&parent_container_id)
                .and_then(|c| self.pid_to_view.get(&c.pid));

            if let Some(parent_view) = parent_view_opt {
                // Strip procmgr-injected per-container infrastructure mounts before
                // checking.  These are private to the child and never represent
                // capability widening relative to the parent:
                //   • MemFs-backed entries (memfs_cid != 0) — always a fresh
                //     per-container MemFs; no parent resource exposed.
                //   • Container-anchored MountTable paths (src starts with
                //     `/var/containers/c-`) — the child's own persistent storage.
                let checkable_mounts: ViewMountList = mounts
                    .iter()
                    .filter(|(src, _, _, memfs_cid)| {
                        *memfs_cid == 0
                            && !src.starts_with("/var/containers/c-")
                    })
                    .cloned()
                    .collect();

                if let Some((bad_src, bad_dst, bad_w)) =
                    first_view_violation(parent_view, &checkable_mounts)
                {
                    let _ = debug_print(&format!(
                        "procmgr: MONOTONE VIOLATION cid={} parent_cid={} \
                         path {}→{} writable={} not covered by parent view",
                        container_id, parent_container_id,
                        bad_src, bad_dst, bad_w
                    ));
                    // In a debug build this is a hard panic so the violation
                    // is caught immediately rather than silently propagating.
                    panic!(
                        "monotone violation: child cid={} parent cid={} \
                         violating path: {}",
                        container_id, parent_container_id, bad_src
                    );
                }
            }
        }
        // ─────────────────────────────────────────────────────────────────────
        self.register_vfs_view_for_thread(thread_token, mounts, profile, container_id);
        if let Err(err) = thread_resume(thread_token) {
            let _ = debug_print(&format!(
                "procmgr: thread_resume failed token={} err={:?}",
                thread_token, err
            ));
            let _ = thread_destroy(thread_token);
        }
    }

    fn register_vfs_view_for_thread(
        &mut self,
        thread_token: usize,
        mounts: &[(String, String, bool, u64)],
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
            self.process_expired_timers();
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

    fn load_envelopes(&mut self) {
        let data = match self.read_file_from_vfs("/etc/envelopes.toml") {
            Some(d) => d,
            None => panic!("procmgr: /etc/envelopes.toml not found — boot cannot continue"),
        };

        let text = match core::str::from_utf8(&data) {
            Ok(s) => s,
            Err(_) => panic!("procmgr: /etc/envelopes.toml is not valid UTF-8"),
        };

        let parsed = match envelopes::parse_envelopes(text) {
            Ok(v) => v,
            Err(e) => panic!("procmgr: envelopes.toml malformed: {}", e),
        };

        self.envelopes = parsed;
        let _ = debug_print(&format!(
            "procmgr: loaded {} envelope(s)", self.envelopes.len()
        ));
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
            let profile_str = match table.get_str("profile") {
                Some(s) => s,
                None => continue,
            };
            let profile = match parse_profile_str(profile_str) {
                Some(p) => p,
                None => continue,
            };
            let escalate = table.get_str("escalate").and_then(parse_profile_str);
            let password = table.get_str("password").map(String::from).unwrap_or_default();
            self.user_records.insert(String::from(username), UserRecord {
                home,
                shell,
                profile,
                profile_name: String::from(profile_str),
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
        if SHELL_AUTOSTART_CMD.is_empty() {
            // Gate diagnostic: production builds (no CLUU_SHELL_AUTOSTART_CMD)
            // never auto-login; harness builds bake a command and do.
            let _ = debug_print("procmgr: autologin skipped (no autostart cmd)");
            self.auto_login_done = true;
            return;
        }
        if self.user_records.is_empty() { return; }
        if self.tty_endpoints[0] == 0 { return; }

        self.auto_login_done = true;
        let _ = debug_print("procmgr: auto-login root on VT:0");

        let (profile, profile_name) = match self.user_records.get("root") {
            Some(r) => (r.profile, r.profile_name.clone()),
            None => return,
        };

        // Resolve envelope and build env. Boot cannot proceed without an
        // envelope for root.
        let envelope = match envelopes::lookup_envelope(&self.envelopes, &profile_name) {
            Some(e) => e.clone(),
            None => {
                panic!(
                    "procmgr: auto-login fail: no envelope for profile '{}'",
                    profile_name
                );
            }
        };
        let view_mounts = Self::build_view_from_envelope(&envelope);
        let resolved_env = envelopes::resolve_env(&envelope, "root");
        let (user_env, user_envc) = build_envelope_env_payload(&resolved_env);

        let spawn_seq = self.next_spawn_seq();
        let spawn_start = self.clock_sample();
        let (shell_argv_payload, shell_argc) = build_shell_argv_payload(SHELL_AUTOSTART_CMD);

        match self.spawn_service_with_env(SERVICE_PATH, DEFAULT_PRIORITY, &shell_argv_payload, shell_argc, &user_env, user_envc, 1, spawn_seq, spawn_start, &[], profile, 0, 0, &[], None, &[], &[], THREAD_CREATE_START_SUSPENDED) {
            Ok((thread_token, _cookie, pid, stdin_send)) => {
                let session_cid = self.next_container_id();
                let shell_cid = self.next_container_id();
                self.pid_to_container_id.insert(pid, shell_cid);
                self.container_owner_pids.insert(pid);
                // parent_container_id=0, is_identity_switch=false: procmgr is the
                // authoritative spawner for auto-login shell; session_cid is a
                // just-allocated local ID, not yet a container with a recorded view.
                self.install_view_and_run(thread_token, &view_mounts, profile, shell_cid, 0, false);
                self.pid_to_view.insert(pid, view_mounts);
                let inst_name = self.next_instance_name(session_cid, "shell");
                self.container_instances.insert(shell_cid, ContainerInstance {
                    name: String::from("shell"),
                    instance_name: inst_name,
                    session_id: session_cid,
                    container_id: shell_cid,
                    parent_container_id: session_cid,
                    pid,
                    image_path: String::from(SERVICE_PATH),
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                    restart_policy: RestartPolicy::Never,
                    restart_count: 0,
                    last_exit_code: 0,
                    restart_attempt_start: 0,
                    quota: QuotaSpec::default(),
                    live_processes: 0,
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

        // Parse restart policy from [lifecycle] section
        let restart_policy = parse_restart_policy(&doc);

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
        // Console opens /dev/fb0 directly; procmgr no longer injects FB params.
        let mut overrides_buf: [(usize, u64); 8] = [(0, 0); 8];
        let mut n_overrides = 0;
        if image_name == "console" {
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
            &[],
            &[], // no redir
            THREAD_CREATE_START_SUSPENDED,
        ) {
            Ok((_thread_token, cookie, pid, _child_stdin_send)) => {
                let image_dir = format!("/var/images/{}", image_name);
                let view_mounts = default_view_for_profile(requested_profile);
                self.pid_to_container_id.insert(pid, container_id);
                self.container_owner_pids.insert(pid);
                // parent_container_id=0, is_identity_switch=false: autostart binaries
                // are top-level; procmgr is root.
                self.install_view_and_run(_thread_token, &view_mounts, requested_profile, container_id, 0, false);
                self.pid_to_view.insert(pid, view_mounts);
                if image_name == "vtmgr" {
                    self.vtmgr_container_id = container_id;
                }
                let inst_name = self.next_instance_name(0, image_name);
                self.container_instances.insert(container_id, ContainerInstance {
                    name: String::from(image_name),
                    instance_name: inst_name,
                    session_id: 0,
                    container_id,
                    parent_container_id: 0, // autostart = top-level
                    pid,
                    image_path: image_dir,
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                    restart_policy,
                    restart_count: 0,
                    last_exit_code: 0,
                    restart_attempt_start: 0,
                    quota: QuotaSpec::default(),
                    live_processes: 0,
                });
                self.autostart_order.push(container_id);
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

    fn should_restart_container(&self, cookie: usize, exit_code: i32) -> bool {
        if self.shutting_down {
            return false;
        }
        let pid = match self.cookie_to_pid.get(&cookie) {
            Some(&p) => p,
            None => return false,
        };
        let container_id = match self.pid_to_container_id.get(&pid) {
            Some(&cid) => cid,
            None => return false,
        };
        let container = match self.container_instances.get(&container_id) {
            Some(c) => c,
            None => return false,
        };
        match &container.restart_policy {
            RestartPolicy::Never => false,
            RestartPolicy::Always => true,
            RestartPolicy::OnFailure { max_restarts, window_secs } => {
                if exit_code == 0 { return false; }
                if container.restart_attempt_start > 0 && *window_secs > 0 {
                    let now = self.clock_sample();
                    let window_ticks = *window_secs * self.clock_freq;
                    if (now - container.restart_attempt_start) <= window_ticks
                       && container.restart_count >= *max_restarts
                    {
                        let _ = debug_print(&format!(
                            "procmgr: crash loop detected for '{}' ({} restarts in {}s window)",
                            container.name, container.restart_count, window_secs
                        ));
                        return false;
                    }
                }
                true
            }
        }
    }

    /// Process all expired timers in the timer queue.
    fn process_expired_timers(&mut self) {
        if self.pending_timers.is_empty() {
            return;
        }
        let now = self.clock_sample();
        // Collect expired restart container IDs.
        let mut expired: Vec<u64> = Vec::new();
        self.pending_timers.retain(|entry| {
            if entry.deadline <= now {
                match &entry.action {
                    TimerAction::Restart(cid) => expired.push(*cid),
                }
                false
            } else {
                true
            }
        });
        for container_id in expired {
            self.pending_restarts.remove(&container_id);
            self.execute_deferred_restart(container_id);
        }
    }

    /// Schedule a deferred restart for a container after `delay_secs` seconds.
    fn schedule_restart_timer(&mut self, container_id: u64, delay_secs: u64) {
        if self.pending_restarts.contains(&container_id) {
            return; // already scheduled
        }
        let deadline = self.clock_sample() + delay_secs * self.clock_freq;
        self.pending_timers.push(TimerEntry {
            deadline,
            action: TimerAction::Restart(container_id),
        });
        self.pending_restarts.insert(container_id);
    }

    /// Execute a deferred restart — re-read manifest and spawn a new process for the container.
    fn execute_deferred_restart(&mut self, container_id: u64) {
        let image_name = match self.container_instances.get(&container_id) {
            Some(c) => c.name.clone(),
            None => return,
        };
        let restart_count = self.container_instances.get(&container_id)
            .map(|c| c.restart_count).unwrap_or(0);
        let _ = debug_print(&format!(
            "procmgr: deferred restart of '{}' (restart #{})",
            image_name, restart_count
        ));
        self.spawn_restarted_container(container_id, &image_name);
    }

    /// Shared spawn logic for container restart (immediate or deferred).
    ///
    /// Re-reads the manifest, parses it, and spawns the binary.
    fn spawn_restarted_container(&mut self, container_id: u64, image_name: &str) {
        let manifest_path = format!("/var/images/{}/manifest.toml", image_name);
        let manifest_contents = match self.read_file_from_vfs(&manifest_path) {
            Some(data) => data,
            None => {
                let _ = debug_print(&format!(
                    "procmgr: restart failed, manifest not found: {}", manifest_path
                ));
                self.container_instances.remove(&container_id);
                return;
            }
        };
        let manifest_str = match core::str::from_utf8(&manifest_contents) {
            Ok(s) => s,
            Err(_) => {
                let _ = debug_print("procmgr: restart failed, manifest not UTF-8");
                self.container_instances.remove(&container_id);
                return;
            }
        };
        let doc = match libcluu::toml::parse(manifest_str) {
            Ok(d) => d,
            Err(e) => {
                let _ = debug_print(&format!(
                    "procmgr: restart failed, manifest parse error: {}", e
                ));
                self.container_instances.remove(&container_id);
                return;
            }
        };

        // Extract spawn parameters (same as autostart_container)
        let binary = match doc.table("exec").and_then(|t| t.get_str("binary")) {
            Some(b) => b,
            None => {
                let _ = debug_print("procmgr: restart failed, manifest missing binary");
                self.container_instances.remove(&container_id);
                return;
            }
        };
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
        let priority = doc.table("scheduling")
            .and_then(|t| t.get_str("priority"))
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_PRIORITY);
        let binary_vfs_path = format!("/var/images/{}{}", image_name, binary);

        // Endpoint setup (same as autostart)
        let endpoint_mode = doc.table("tokens").and_then(|t| t.get_str("endpoint_mode"));
        let extra_token = match endpoint_mode {
            Some("listen") => {
                let ep = endpoint_create(self.token).unwrap_or(0);
                if ep != 0 {
                    token_derive(ep, Rights::IPC_RECV.bits() as usize, u64::MAX).unwrap_or(0)
                } else { 0 }
            }
            Some("grantable") => {
                let ep = endpoint_create(self.token).unwrap_or(0);
                if ep != 0 {
                    let rights = Rights::IPC_RECV | Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT;
                    token_derive(ep, rights.bits() as usize, u64::MAX).unwrap_or(0)
                } else { 0 }
            }
            _ => 0,
        };
        let devices: Vec<String> = doc.table("hardware")
            .and_then(|t| t.get_array("devices"))
            .map(|a| a.iter().map(|s| s.clone()).collect())
            .unwrap_or_default();
        let extra_token_1 = if devices.iter().any(|d| d == "irq") {
            token_derive(self.token, Rights::IRQ_HANDLE.bits() as usize, u64::MAX).unwrap_or(0)
        } else { 0 };

        let mut argv_payload: Vec<u8> = Vec::new();
        argv_payload.extend_from_slice(binary.as_bytes());
        argv_payload.push(0);

        // Console-specific param overrides (VT instance/active only).
        // Console opens /dev/fb0 directly; procmgr no longer injects FB params.
        let mut overrides_buf: [(usize, u64); 8] = [(0, 0); 8];
        let mut n_overrides = 0;
        if image_name == "console" {
            overrides_buf[n_overrides] = (PARAM_CONSOLE_INSTANCE, 0);
            n_overrides += 1;
            overrides_buf[n_overrides] = (PARAM_CONSOLE_ACTIVE, 1);
            n_overrides += 1;
        }
        let param_overrides = &overrides_buf[..n_overrides];

        let spawn_seq = self.next_spawn_seq();
        let spawn_start = self.clock_sample();

        match self.spawn_service_with_env(
            &binary_vfs_path, priority, &argv_payload, 1, &[], 0, 0,
            spawn_seq, spawn_start, &[], requested_profile,
            extra_token, extra_token_1, param_overrides, None, &[], &[],
            THREAD_CREATE_START_SUSPENDED,
        ) {
            Ok((new_thread_token, _new_cookie, new_pid, _)) => {
                let view_mounts = default_view_for_profile(requested_profile);
                // Restart: container_instances[container_id] already exists —
                // extract the recorded parent before calling install_view_and_run.
                // For top-level autostart containers parent is 0 (correct: skip assertion).
                let restart_parent_cid = self.container_instances
                    .get(&container_id)
                    .map(|c| c.parent_container_id)
                    .unwrap_or(0);
                self.pid_to_container_id.insert(new_pid, container_id);
                self.container_owner_pids.insert(new_pid);
                self.install_view_and_run(new_thread_token, &view_mounts, requested_profile, container_id, restart_parent_cid, false);
                self.pid_to_view.insert(new_pid, view_mounts);

                if let Some(container) = self.container_instances.get_mut(&container_id) {
                    container.pid = new_pid;
                }

                let _ = debug_print(&format!(
                    "procmgr: '{}' restarted as pid {} (container {})",
                    image_name, new_pid, container_id
                ));
            }
            Err(e) => {
                let _ = debug_print(&format!(
                    "procmgr: restart spawn failed for '{}': {:?}", image_name, e
                ));
                self.container_instances.remove(&container_id);
            }
        }
    }

    fn handle_restart_exit(&mut self, cookie: usize, exit_code: i32) -> Result<()> {
        // 1. Extract state before cleanup
        let thread_token = self.exit_table.remove(&cookie).unwrap_or(0);
        let pid = match self.cookie_to_pid.remove(&cookie) {
            Some(p) => p,
            None => return Ok(()),
        };
        let child_tid = self.pid_to_tid.get(&pid).copied().unwrap_or(0);
        let container_id = match self.pid_to_container_id.remove(&pid) {
            Some(cid) => cid,
            None => return Ok(()),
        };
        if let Some(container) = self.container_instances.get_mut(&container_id) {
            container.live_processes = container.live_processes.saturating_sub(1);
        }

        // 2. Clean up PROCESS resources (not container)
        self.clear_vfs_view_for_tid(child_tid);
        self.pid_to_cookie.remove(&pid);
        self.clear_pid_runtime_state(pid);
        if let Some(owner_tid) = self.pid_owner_tid.remove(&pid) {
            self.on_child_reaped(owner_tid);
        }
        // Notify parent of exit (they see the exit, restart is internal)
        if let Some(notify_ep) = self.exit_notify.remove(&cookie) {
            let mut notify_msg = Message::new(PROCMGR_EXIT_LABEL, [0; 6], 2);
            notify_msg.words[0] = cookie;
            notify_msg.words[1] = exit_code as usize;
            let _ = send(notify_ep, &notify_msg, IpcFlags::empty());
        }

        // 3. Destroy kernel objects
        if thread_token != 0 { let _ = thread_destroy(thread_token); }
        if let Some(st) = self.cookie_to_space.remove(&cookie) {
            let _ = space_destroy(st);
        }
        if let Some(tokens) = self.cookie_to_tokens.remove(&cookie) {
            for tok in tokens { let _ = token_revoke(tok); }
        }

        // 4. Update restart metadata and determine restart strategy
        let now = self.clock_sample();
        let freq = self.clock_freq;
        let (image_name, restart_count, policy) = {
            let container = match self.container_instances.get_mut(&container_id) {
                Some(c) => c,
                None => return Ok(()),
            };
            container.pid = 0;
            container.last_exit_code = exit_code;

            // Determine window for counter reset (applies to both Always and OnFailure)
            let window_secs = match &container.restart_policy {
                RestartPolicy::OnFailure { window_secs, .. } => *window_secs,
                RestartPolicy::Always => ALWAYS_SAFETY_VALVE_WINDOW_SECS,
                RestartPolicy::Never => 0,
            };

            // Reset counters if outside window (fresh restart cycle)
            if container.restart_attempt_start > 0
               && window_secs > 0
               && (now - container.restart_attempt_start) > window_secs * freq
            {
                container.restart_count = 0;
                container.restart_attempt_start = now;
            } else if container.restart_attempt_start == 0 {
                container.restart_attempt_start = now;
            }
            container.restart_count += 1;

            (container.name.clone(), container.restart_count, container.restart_policy.clone())
        };

        let _ = debug_print(&format!(
            "procmgr: restarting '{}' (exit code {}, restart #{})",
            image_name, exit_code, restart_count
        ));

        // 5. Dispatch: immediate or deferred restart based on policy
        match &policy {
            RestartPolicy::Always => {
                // Safety valve: if too many rapid restarts, apply exponential backoff
                if restart_count > ALWAYS_SAFETY_VALVE_COUNT {
                    let exponent = ((restart_count - ALWAYS_SAFETY_VALVE_COUNT - 1) as u64).min(4);
                    let delay = (ONFAILURE_BACKOFF_BASE_SECS << exponent).min(ONFAILURE_BACKOFF_CAP_SECS);
                    let _ = debug_print(&format!(
                        "procmgr: safety valve for '{}', deferring {}s (restart #{})",
                        image_name, delay, restart_count
                    ));
                    self.schedule_restart_timer(container_id, delay);
                } else {
                    // Immediate restart
                    self.spawn_restarted_container(container_id, &image_name);
                }
            }
            RestartPolicy::OnFailure { .. } => {
                // Exponential backoff: 1s, 2s, 4s, 8s, 16s, cap 30s
                let exponent = (restart_count.saturating_sub(1) as u64).min(4);
                let delay = (ONFAILURE_BACKOFF_BASE_SECS << exponent).min(ONFAILURE_BACKOFF_CAP_SECS);
                let _ = debug_print(&format!(
                    "procmgr: OnFailure backoff for '{}', deferring {}s (restart #{})",
                    image_name, delay, restart_count
                ));
                self.schedule_restart_timer(container_id, delay);
            }
            RestartPolicy::Never => {} // should_restart_container already filters this
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
        // Buffer must fit the largest message procmgr can receive on any of
        // its four endpoints. PROCMGR_SPAWN_LABEL with FDAC + envp + cwd
        // trailer can exceed 256 bytes; kernel `recv_to_user_nonblocking`
        // pops the message and returns BufferTooSmall if it overflows,
        // dropping the message permanently. 4 KiB matches the inline IPC
        // upper bound used elsewhere in libcluu.
        let mut buf = [0u8; 4096];
        // Compute timeout: wake up when the soonest timer expires (or block forever).
        let timeout = if self.pending_timers.is_empty() {
            u64::MAX
        } else {
            let now = self.clock_sample();
            let soonest = self.pending_timers.iter().map(|t| t.deadline).min().unwrap();
            if soonest <= now { 0 } else { soonest - now }
        };
        let (index, len, sender_tid) =
            match libcluu::syscall::ipc_recv_any_with_sender(&tokens, &mut buf, timeout) {
                Ok(res) => res,
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

        // Check if this container should be restarted instead of torn down
        if self.should_restart_container(cookie, exit_code) {
            return self.handle_restart_exit(cookie, exit_code);
        }

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
            if let Some(container) = self.container_instances.get_mut(&container_id) {
                container.live_processes = container.live_processes.saturating_sub(1);
            }
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
                    self.load_envelopes();
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
                    if let Some(container) = self.container_instances.get_mut(&container_id) {
                        container.live_processes = container.live_processes.saturating_sub(1);
                    }
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
            return self.handle_container_list(msg, sender_tid);
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
        if msg.tag.label == libcluu::ipc::PROCMGR_SHUTDOWN_LABEL {
            return self.handle_shutdown(msg, sender_tid);
        }
        if msg.tag.label == PROCMGR_PROC_QUERY_LABEL {
            return self.handle_proc_query(msg, sender_tid);
        }
        if msg.tag.label == libcluu::ipc::PROCMGR_PIPE_CREATE_LABEL {
            return self.handle_pipe_create(msg, sender_tid);
        }
        if msg.tag.label == libcluu::ipc::PROCMGR_PIPE_CLOSE_LABEL {
            return self.handle_pipe_close(msg, sender_tid);
        }
        // Phase 4 Plan D — job control
        if msg.tag.label == PROCMGR_PG_CREATE_LABEL {
            return self.handle_pg_create(msg);
        }
        if msg.tag.label == PROCMGR_PG_ATTACH_LABEL {
            return self.handle_pg_attach(msg);
        }
        if msg.tag.label == PROCMGR_PG_SUSPEND_LABEL {
            return self.handle_pg_suspend(msg);
        }
        if msg.tag.label == PROCMGR_PG_RESUME_LABEL {
            return self.handle_pg_resume(msg);
        }
        if msg.tag.label == PROCMGR_PG_SIGNAL_LABEL {
            return self.handle_pg_signal(msg);
        }
        if msg.tag.label == PROCMGR_PID_PGID_QUERY_LABEL {
            return self.handle_pid_pgid_query(msg);
        }
        self.handle_spawn_message(msg, payload, sender_tid)
    }

    fn is_sender_kbd_service(&self, sender_tid: usize) -> bool {
        let pid = match self.tid_to_pid.get(&sender_tid) {
            Some(&p) => p,
            None => return false,
        };
        let cid = match self.pid_to_container_id.get(&pid) {
            Some(&c) => c,
            None => return false,
        };
        match self.container_instances.get(&cid) {
            Some(inst) => inst.name == "kbd",
            None => false,
        }
    }

    fn handle_shutdown(&mut self, msg: &Message, sender_tid: usize) -> Result<()> {
        // Auth: ADMIN profile or kbd service
        let sender_pid = self.tid_to_pid.get(&sender_tid).copied().unwrap_or(0);
        let is_admin = self.pid_to_profile.get(&sender_pid)
            .map(|p| p.contains(CapProfile::ADMIN))
            .unwrap_or(false);
        if !is_admin && !self.is_sender_kbd_service(sender_tid) {
            let _ = debug_print(&format!(
                "procmgr: shutdown rejected, sender tid={} not authorized", sender_tid
            ));
            return Err(Error::PermissionDenied);
        }

        self.shutting_down = true;
        self.shutdown_action = msg.words[0] as u8; // 0=poweroff, 1=reboot
        // Cancel all pending deferred restarts.
        self.pending_timers.clear();
        self.pending_restarts.clear();
        let action_str = if self.shutdown_action == 1 { "reboot" } else { "poweroff" };
        let _ = debug_print(&format!("procmgr: shutdown initiated ({})", action_str));

        self.shutdown_kill_sessions();
        self.shutdown_kill_tier2();
        self.shutdown_flush_vfs();

        let exit_code = if self.shutdown_action == 1 { 43 } else { 42 };
        let _ = debug_print(&format!("procmgr: shutdown complete, exiting with code {}", exit_code));

        // Exit procmgr — init will detect via primordial exit monitoring
        let _ = libcluu::ipc::notify_exit(exit_code);
        loop { let _ = yield_cpu(); }
    }

    fn shutdown_kill_sessions(&mut self) {
        let session_cids: Vec<u64> = self.session_table.values().map(|s| s.shell_cid).collect();
        let _ = debug_print(&format!("procmgr: shutdown killing {} sessions", session_cids.len()));

        for shell_cid in session_cids {
            self.destroy_container_children(shell_cid);
            if let Some(inst) = self.container_instances.remove(&shell_cid) {
                self.kill_container_process(inst.pid, shell_cid);
            }
        }
        self.session_table.clear();
        self.vt_to_session = [0; VT_COUNT];
    }

    fn shutdown_kill_tier2(&mut self) {
        let order: Vec<u64> = self.autostart_order.iter().rev().copied().collect();
        let _ = debug_print(&format!(
            "procmgr: shutdown killing {} Tier 2 services (reverse order)", order.len()
        ));

        for cid in order {
            if let Some(inst) = self.container_instances.remove(&cid) {
                let _ = debug_print(&format!(
                    "procmgr: shutdown killing service '{}' (cid={})", inst.name, cid
                ));
                self.destroy_container_children(cid);
                self.kill_container_process(inst.pid, cid);
            }
        }
        self.autostart_order.clear();
    }

    fn shutdown_flush_vfs(&self) {
        if self.vfs_endpoint == 0 {
            let _ = debug_print("procmgr: shutdown skipping VFS flush (no endpoint)");
            return;
        }
        let _ = debug_print("procmgr: shutdown flushing VFS");
        let msg = Message::new(libcluu::fs::protocol::VFS_FLUSH, [0; 6], 0);
        let _ = send(self.vfs_endpoint, &msg, IpcFlags::empty());
    }

    fn kill_container_process(&mut self, pid: usize, container_id: u64) {
        if let Some(&cookie) = self.pid_to_cookie.get(&pid) {
            if let Some(thread_token) = self.exit_table.remove(&cookie) {
                let _ = thread_destroy(thread_token);
            }
            if let Some(st) = self.cookie_to_space.remove(&cookie) {
                let _ = space_destroy(st);
            }
            if let Some(tokens) = self.cookie_to_tokens.remove(&cookie) {
                for tok in tokens {
                    let _ = token_revoke(tok);
                }
            }
            let tid = self.pid_to_tid.get(&pid).copied().unwrap_or(0);
            self.clear_vfs_view_for_tid(tid);
            self.pid_to_cookie.remove(&pid);
            self.cookie_to_pid.remove(&cookie);
            self.pid_to_container_id.remove(&pid);
            self.clear_pid_runtime_state(pid);
            self.exit_notify.remove(&cookie);
            self.pid_owner_tid.remove(&pid);
            self.container_owner_pids.remove(&pid);
        }
        if container_id > 0 {
            let _ = send_vfs_container_cleanup(self.vfs_endpoint, container_id, 1);
        }
    }

    #[allow(dead_code)]
    fn build_session_view(&self, user_record: &UserRecord) -> ViewMountList {
        self.build_view_for_profile_and_home(user_record.profile, &user_record.home)
    }

    fn handle_session_login(&mut self, msg: &Message, payload: &[u8], _sender_tid: usize) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(libcluu::ipc::PROCMGR_SESSION_LOGIN_LABEL, [0; 6], 5);
        let vt_index = msg.words[1];

        // Read session_kind from the first byte: 0 = tty, 1 = compositor.
        // Both tty/context.rs and login/src/main.rs now send this prefix byte.
        let (session_kind, cred_payload) = match payload.split_first() {
            Some((&k, rest)) if k <= 1 => (k, rest),
            _ => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // session_kind=1 (compositor): validate credentials, resolve user envelope,
        // spawn cluuterm under the authenticated user's profile, track session.
        if session_kind == 1 {
            // Parse username\0password\0 from cred_payload.
            // Own both as String immediately so VFS IPC cannot trash the recv buffer.
            let payload_str = core::str::from_utf8(cred_payload).unwrap_or("");
            let mut parts = payload_str.splitn(3, '\0');
            let username: alloc::string::String = match parts.next() {
                Some(u) if !u.is_empty() => alloc::string::String::from(u),
                _ => {
                    reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                    if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                    return Ok(());
                }
            };
            let password: alloc::string::String =
                alloc::string::String::from(parts.next().unwrap_or(""));

            // Rate-limit check.
            if self.is_rate_limited(&username) {
                let _ = debug_print(&format!(
                    "procmgr: SESSION_LOGIN session_kind=1 rate-limited for '{}'", username
                ));
                self.audit_log("WARN", "AUTH_LOGIN_RATE", &format!("user={}", username));
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }

            // Look up user record — extract owned values so reference is dropped.
            let (stored_pw, profile, profile_name) =
                match self.user_records.get(username.as_str()) {
                    Some(r) => (r.password.clone(), r.profile, r.profile_name.clone()),
                    None => {
                        let _ = debug_print(&format!(
                            "procmgr: SESSION_LOGIN session_kind=1 unknown user '{}'", username
                        ));
                        self.audit_log(
                            "WARN",
                            "AUTH_LOGIN_FAIL",
                            &format!("user={} reason=unknown_user kind=1", username),
                        );
                        self.record_auth_failure(&username);
                        reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                        if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                        return Ok(());
                    }
                };

            // Verify password (no reference to self.user_records alive).
            if !crypto::verify_password(&password, &stored_pw) {
                let _ = debug_print(&format!(
                    "procmgr: SESSION_LOGIN session_kind=1 bad password for '{}'", username
                ));
                self.audit_log(
                    "WARN",
                    "AUTH_LOGIN_FAIL",
                    &format!("user={} reason=bad_password kind=1", username),
                );
                self.record_auth_failure(&username);
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }

            self.clear_auth_failures(&username);
            self.audit_log(
                "INFO",
                "AUTH_LOGIN_OK",
                &format!("user={} kind=1", username),
            );

            // Resolve user envelope.
            let envelope = match envelopes::lookup_envelope(&self.envelopes, &profile_name) {
                Some(e) => e.clone(),
                None => {
                    let _ = debug_print(&format!(
                        "procmgr: SESSION_LOGIN session_kind=1 no envelope for profile '{}'",
                        profile_name
                    ));
                    self.audit_log(
                        "WARN",
                        "AUTH_LOGIN_FAIL",
                        &format!("user={} reason=no_envelope kind=1", username),
                    );
                    reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                    if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                    return Ok(());
                }
            };
            // TODO: thread vt through from caller
            let vt_index_graphical = 4usize;
            let resolved_mounts = envelopes::resolve_session_mounts(
                &envelope, /* session_kind */ 1, vt_index_graphical, &username,
            );
            let view_mounts = Self::build_view_from_mount_strings(&resolved_mounts);
            let resolved_env = envelopes::resolve_env(&envelope, &username);
            let (user_env, user_envc) = build_envelope_env_payload(&resolved_env);

            // Build a minimal argv for cluuterm: just the binary name.
            let mut cluuterm_argv: Vec<u8> = Vec::new();
            cluuterm_argv.extend_from_slice(b"cluuterm\0");
            let cluuterm_argc = 1usize;

            const CLUUTERM_BIN: &str = "/var/images/cluuterm/bin/cluuterm";

            let spawn_seq = self.next_spawn_seq();
            let spawn_start = self.clock_sample();

            match self.spawn_service_with_env(
                CLUUTERM_BIN,
                DEFAULT_PRIORITY,
                &cluuterm_argv,
                cluuterm_argc,
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
                None,  // no caller view
                &[],
                &[],   // no redir
                THREAD_CREATE_START_SUSPENDED,
            ) {
                Ok((thread_token, _cookie, pid, _stdin_send)) => {
                    let session_cid = self.next_container_id();
                    let cluuterm_cid = self.next_container_id();
                    self.pid_to_container_id.insert(pid, cluuterm_cid);
                    self.container_owner_pids.insert(pid);

                    // parent_container_id=0, is_identity_switch=false: procmgr is the
                    // authoritative spawner for graphical session cluuterm; session_cid is
                    // newly allocated, no view yet.
                    self.install_view_and_run(thread_token, &view_mounts, profile, cluuterm_cid, 0, false);
                    self.pid_to_view.insert(pid, view_mounts);

                    let inst_name = self.next_instance_name(session_cid, "cluuterm");
                    self.container_instances.insert(cluuterm_cid, ContainerInstance {
                        name: String::from("cluuterm"),
                        instance_name: inst_name,
                        session_id: session_cid,
                        container_id: cluuterm_cid,
                        parent_container_id: session_cid,
                        pid,
                        image_path: String::from("/var/images/cluuterm"),
                        mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                        restart_policy: RestartPolicy::Never,
                        restart_count: 0,
                        last_exit_code: 0,
                        restart_attempt_start: 0,
                        quota: QuotaSpec::default(),
                        live_processes: 0,
                    });

                    self.container_children.entry(session_cid)
                        .or_insert_with(Vec::new).push(cluuterm_cid);

                    self.session_table.insert(session_cid, SessionEntry {
                        container_id: session_cid,
                        shell_cid: cluuterm_cid,
                        pid,
                        username: username.clone(),
                        profile,
                        vt_index: usize::MAX, // compositor session — no legacy VT
                        stdin_endpoint: 0,    // cluuterm manages its own I/O
                    });

                    let _ = debug_print(&format!(
                        "procmgr: SESSION_LOGIN session_kind=1 user='{}' pid={} session_cid={} cluuterm_cid={}",
                        username, pid, session_cid, cluuterm_cid
                    ));

                    reply_msg.words[0] = 0;
                    reply_msg.words[1] = session_cid as usize;
                }
                Err(e) => {
                    let _ = debug_print(&format!(
                        "procmgr: SESSION_LOGIN session_kind=1 spawn failed: {:?}", e
                    ));
                    reply_msg.words[0] = e.to_errno() as usize;
                }
            }

            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // session_kind=0 (tty): existing credential-verification + session-spawn path.
        // Parse username\0password\0 from cred_payload.
        // Own both as String immediately so that spawn_service_with_env (which
        // calls VFS IPC) cannot trash the kernel recv buffer they point into.
        let payload_str = core::str::from_utf8(cred_payload).unwrap_or("");
        let mut parts = payload_str.splitn(3, '\0');
        let username: alloc::string::String = match parts.next() {
            Some(u) if !u.is_empty() => alloc::string::String::from(u),
            _ => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };
        let password: alloc::string::String = alloc::string::String::from(parts.next().unwrap_or(""));

        // Rate limit check (before any password verification)
        if self.is_rate_limited(&username) {
            let _ = debug_print(&format!("procmgr: login rate-limited for '{}'", username));
            self.audit_log("WARN", "AUTH_LOGIN_RATE", &format!("user={}", username));
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // Look up user record — extract all owned values so reference is dropped
        let (stored_pw, profile, profile_name) = match self.user_records.get(username.as_str()) {
            Some(r) => (r.password.clone(), r.profile, r.profile_name.clone()),
            None => {
                let _ = debug_print(&format!("procmgr: login failed, unknown user '{}'", username));
                self.audit_log("WARN", "AUTH_LOGIN_FAIL", &format!("user={} reason=unknown_user", username));
                self.record_auth_failure(&username);
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Verify password (no reference to self.user_records alive)
        if !crypto::verify_password(&password, &stored_pw) {
            let _ = debug_print(&format!("procmgr: login rejected: bad password for '{}'", username));
            self.audit_log("WARN", "AUTH_LOGIN_FAIL", &format!("user={} vt={} reason=bad_password", username, vt_index));
            self.record_auth_failure(&username);
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        self.clear_auth_failures(&username);
        self.audit_log("INFO", "AUTH_LOGIN_OK", &format!("user={} vt={}", username, vt_index));
        let _ = debug_print(&format!("procmgr: session login user='{}' vt={}", username, vt_index));

        // Reject if a session is already active on this VT.
        // vt_index >= VT_COUNT means "pts-backed / non-legacy-VT login".
        // The caller (e.g. cluuterm's /bin/login) manages its own I/O via a
        // pts fd; procmgr only needs to verify credentials, not create a VT
        // session.  Return OK immediately so the caller can emit its marker
        // and posix_spawn its own shell.
        if vt_index >= VT_COUNT {
            let _ = debug_print(&format!(
                "procmgr: auth-only login user='{}' (non-VT pts, vt={})",
                username, vt_index
            ));
            reply_msg.words[0] = 0;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        if self.vt_to_session[vt_index] != 0 {
            let _ = debug_print(&format!("procmgr: login rejected, vt={} has active session", vt_index));
            reply_msg.words[0] = Error::AlreadyExists.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        let tty_ep = self.tty_endpoints[vt_index];
        if tty_ep == 0 {
            let _ = debug_print(&format!("procmgr: login vt={}: no tty endpoint", vt_index));
            reply_msg.words[0] = Error::NotFound.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // Resolve envelope after authentication; reject login if not found.
        let envelope = match envelopes::lookup_envelope(&self.envelopes, &profile_name) {
            Some(e) => e.clone(),
            None => {
                let _ = debug_print(&format!(
                    "procmgr: session-login fail: no envelope for profile '{}'",
                    profile_name
                ));
                self.audit_log(
                    "WARN",
                    "AUTH_LOGIN_FAIL",
                    &format!("user={} reason=no_envelope", username),
                );
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };
        let resolved_mounts = envelopes::resolve_session_mounts(
            &envelope, /* session_kind */ 0, vt_index, &username,
        );
        let view_mounts = Self::build_view_from_mount_strings(&resolved_mounts);
        let resolved_env = envelopes::resolve_env(&envelope, &username);

        let spawn_seq = self.next_spawn_seq();
        let spawn_start = self.clock_sample();
        let (shell_argv_payload, shell_argc) = build_shell_argv_payload(SHELL_AUTOSTART_CMD);
        let (user_env, user_envc) = build_envelope_env_payload(&resolved_env);

        // Open /dev/tty<vt> via procmgr's VFS client so the child shell's
        // fd 0/1/2 are VFS-backed handles to the tty service, served via
        // TTY_READ_REQUEST_LABEL on read(2). This retires the old
        // TOKEN_STDIN push pattern for the legacy VT path.
        let (tty_client_id, tty_remote_fd) = match self.open_dev_tty_for_session(vt_index) {
            Ok(pair) => pair,
            Err(e) => {
                let _ = debug_print(&format!(
                    "procmgr: open /dev/tty{} failed: {:?}; aborting login",
                    vt_index, e
                ));
                reply_msg.words[0] = e.to_errno() as usize;
                if let Some(tok) = reply_token {
                    let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        };
        let fdac_data = self.build_devtty_fdac(tty_client_id, tty_remote_fd);
        let procmgr_main_tid = match self.procmgr_main_tid() {
            Ok(t) => t,
            Err(e) => {
                let _ = debug_print(&format!(
                    "procmgr: procmgr_main_tid lookup failed: {:?}", e
                ));
                reply_msg.words[0] = e.to_errno() as usize;
                if let Some(tok) = reply_token {
                    let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        };

        // Note: we no longer need to temporarily swap self.tty_endpoints[0]
        // because FDAC fully describes fd 0/1/2. Keep the swap commented in
        // case future stdout/log routing wants a fallback when FDAC parse
        // fails — for now, FDAC is the only source of truth.
        let _ = tty_ep; // suppress unused-warning until removed in Task 9

        match self.spawn_service_with_env(
            SERVICE_PATH,
            DEFAULT_PRIORITY,
            &shell_argv_payload,
            shell_argc,
            &user_env,
            user_envc,
            procmgr_main_tid, // VFS will see opens under this tid
            spawn_seq,
            spawn_start,
            &fdac_data,
            profile,
            0,
            0,
            &[],
            None,
            &[],
            &[], // no redir
            THREAD_CREATE_START_SUSPENDED,
        ) {
            Ok((thread_token, _cookie, pid, stdin_send)) => {
                let session_cid = self.next_container_id();
                let shell_cid = self.next_container_id();
                self.pid_to_container_id.insert(pid, shell_cid);
                self.container_owner_pids.insert(pid);

                // parent_container_id=0, is_identity_switch=false: procmgr is the
                // authoritative spawner for tty session shell; session_cid is newly
                // allocated, no view yet.
                self.install_view_and_run(thread_token, &view_mounts, profile, shell_cid, 0, false);
                self.pid_to_view.insert(pid, view_mounts);

                let inst_name = self.next_instance_name(session_cid, "shell");
                self.container_instances.insert(shell_cid, ContainerInstance {
                    name: String::from("shell"),
                    instance_name: inst_name,
                    session_id: session_cid,
                    container_id: shell_cid,
                    parent_container_id: session_cid,
                    pid,
                    image_path: String::from(SERVICE_PATH),
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                    restart_policy: RestartPolicy::Never,
                    restart_count: 0,
                    last_exit_code: 0,
                    restart_attempt_start: 0,
                    quota: QuotaSpec::default(),
                    live_processes: 0,
                });

                self.container_children.entry(session_cid)
                    .or_insert_with(Vec::new).push(shell_cid);

                self.session_table.insert(session_cid, SessionEntry {
                    container_id: session_cid,
                    shell_cid,
                    pid,
                    username: username.clone(),
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
        // Own command_path as String immediately — spawn_service_with_env calls
        // VFS IPC which overwrites the kernel recv buffer that payload_str
        // points into.
        let payload_str = core::str::from_utf8(payload).unwrap_or("");
        let mut parts = payload_str.splitn(3, '\0');
        let password = alloc::string::String::from(parts.next().unwrap_or(""));
        let command_path: alloc::string::String = match parts.next() {
            Some(p) if !p.is_empty() => alloc::string::String::from(p),
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

        // Rate limit check
        if self.is_rate_limited(&username) {
            let _ = debug_print(&format!("procmgr: escalate rate-limited for '{}'", username));
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // Look up user record — extract owned values so reference is dropped
        let (stored_pw, escalate_profile) = match self.user_records.get(&username) {
            Some(r) => (r.password.clone(), r.escalate),
            None => {
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Verify password (no reference to self.user_records alive)
        if !crypto::verify_password(&password, &stored_pw) {
            let _ = debug_print(&format!(
                "procmgr: escalate rejected: bad password for '{}'", username
            ));
            self.audit_log("WARN", "AUTH_SUDO_FAIL", &format!("user={} reason=bad_password", username));
            self.record_auth_failure(&username);
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }
        self.clear_auth_failures(&username);
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

        self.audit_log("INFO", "AUTH_SUDO_OK", &format!("user={}", username));
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
        let basename = command_path.rsplit('/').next().unwrap_or(command_path.as_str());
        let mut argv_payload = Vec::new();
        argv_payload.extend_from_slice(basename.as_bytes());
        argv_payload.push(0);
        let argc = 1usize;
        let (esc_env, esc_envc) = build_user_env_payload(&username, &user_home);

        // Look up caller's VFS view for path resolution
        let caller_view_owned = self.pid_to_view.get(&caller_pid).cloned();

        match self.spawn_service_with_env(
            &command_path,
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
            &[],
            &[], // no redir
            THREAD_CREATE_START_SUSPENDED,
        ) {
            Ok((thread_token, cookie, pid, stdin_send)) => {
                let container_id = self.next_container_id();
                self.pid_to_container_id.insert(pid, container_id);
                self.container_owner_pids.insert(pid);
                // is_identity_switch=true: escalate is an authenticated identity switch;
                // the target view is authorized by procmgr policy, not constrained by
                // the caller's view.  Pass caller_container_id for audit trail.
                self.install_view_and_run(thread_token, &view_mounts, escalate_profile, container_id, caller_container_id, true);
                self.pid_to_view.insert(pid, view_mounts);

                let sudo_session_id = self.resolve_caller_session(sender_tid)
                    .map(|s| s.container_id).unwrap_or(0);
                let sudo_name = format!("sudo:{}", username);
                let inst_name = self.next_instance_name(sudo_session_id, &sudo_name);
                self.container_instances.insert(container_id, ContainerInstance {
                    name: sudo_name,
                    instance_name: inst_name,
                    session_id: sudo_session_id,
                    container_id,
                    parent_container_id: caller_container_id,
                    pid,
                    image_path: String::from(command_path),
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                    restart_policy: RestartPolicy::Never,
                    restart_count: 0,
                    last_exit_code: 0,
                    restart_attempt_start: 0,
                    quota: QuotaSpec::default(),
                    live_processes: 0,
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
        // Own both as String immediately — spawn_service_with_env calls VFS IPC
        // which overwrites the kernel recv buffer that payload_str borrows from.
        let payload_str = core::str::from_utf8(payload).unwrap_or("");
        let mut parts = payload_str.splitn(3, '\0');
        let target_username: alloc::string::String = match parts.next() {
            Some(u) if !u.is_empty() => alloc::string::String::from(u),
            _ => {
                let _ = debug_print("procmgr: su rejected: missing target username");
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };
        let password: alloc::string::String = alloc::string::String::from(parts.next().unwrap_or(""));

        // Rate limit check
        if self.is_rate_limited(&target_username) {
            let _ = debug_print(&format!("procmgr: su rate-limited for '{}'", target_username));
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // Look up target user record — extract owned values so reference is dropped
        let (stored_pw, target_profile, target_profile_name) = match self.user_records.get(target_username.as_str()) {
            Some(record) => (
                record.password.clone(),
                record.profile,
                record.profile_name.clone(),
            ),
            None => {
                let _ = debug_print(&format!(
                    "procmgr: su rejected: unknown user '{}'", target_username
                ));
                self.record_auth_failure(&target_username);
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Verify target user's password (no reference to self.user_records alive)
        if !crypto::verify_password(&password, &stored_pw) {
            let _ = debug_print(&format!(
                "procmgr: su rejected: bad password for '{}'", target_username
            ));
            let caller_user = self.resolve_caller_session(sender_tid)
                .map(|s| s.username.clone()).unwrap_or_else(|| String::from("?"));
            self.audit_log("WARN", "AUTH_SU_FAIL", &format!("from={} to={} reason=bad_password", caller_user, target_username));
            self.record_auth_failure(&target_username);
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }
        self.clear_auth_failures(&target_username);

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

        let caller_user = self.resolve_caller_session(sender_tid)
            .map(|s| s.username.clone()).unwrap_or_else(|| String::from("?"));
        self.audit_log("INFO", "AUTH_SU_OK", &format!("from={} to={}", caller_user, target_username));
        let _ = debug_print(&format!(
            "procmgr: su target='{}' profile={:#x} (caller={:#x})",
            target_username, target_profile.bits(), caller_profile.bits()
        ));

        // Resolve target user's envelope; reject su if not found.
        let envelope = match envelopes::lookup_envelope(&self.envelopes, &target_profile_name) {
            Some(e) => e.clone(),
            None => {
                let _ = debug_print(&format!(
                    "procmgr: su rejected: no envelope for profile '{}'",
                    target_profile_name
                ));
                self.audit_log(
                    "WARN",
                    "AUTH_SU_FAIL",
                    &format!("from={} to={} reason=no_envelope", caller_user, target_username),
                );
                reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };
        // Build view from envelope mounts (UE10 pattern).
        let view_mounts = Self::build_view_from_envelope(&envelope);
        let resolved_env = envelopes::resolve_env(&envelope, &target_username);

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
        let (su_env, su_envc) = build_envelope_env_payload(&resolved_env);

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
            &[],
            &[], // no redir
            THREAD_CREATE_START_SUSPENDED,
        ) {
            Ok((thread_token, cookie, pid, stdin_send)) => {
                let container_id = self.next_container_id();
                self.pid_to_container_id.insert(pid, container_id);
                self.container_owner_pids.insert(pid);
                // is_identity_switch=true: su is an authenticated identity switch;
                // the target view is authorized by procmgr policy, not constrained by
                // the caller's view.  Pass caller_container_id for audit trail.
                self.install_view_and_run(thread_token, &view_mounts, target_profile, container_id, caller_container_id, true);
                self.pid_to_view.insert(pid, view_mounts);

                let su_session_id = self.resolve_caller_session(sender_tid)
                    .map(|s| s.container_id).unwrap_or(0);
                let su_name = format!("su:{}", target_username);
                let inst_name = self.next_instance_name(su_session_id, &su_name);
                self.container_instances.insert(container_id, ContainerInstance {
                    name: su_name,
                    instance_name: inst_name,
                    session_id: su_session_id,
                    container_id,
                    parent_container_id: caller_container_id,
                    pid,
                    image_path: String::from(SERVICE_PATH),
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                    restart_policy: RestartPolicy::Never,
                    restart_count: 0,
                    last_exit_code: 0,
                    restart_attempt_start: 0,
                    quota: QuotaSpec::default(),
                    live_processes: 0,
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

    /// Handle PROCMGR_PIPE_CREATE_LABEL: allocate a new IPC-endpoint-backed pipe.
    ///
    /// Creates a fresh endpoint, mints an IPC_SEND token (write end) and an
    /// IPC_RECV token (read end), stores them in the pipe table, and replies
    /// with [status=0, write_token, read_token, pipe_id].  On any failure the
    /// partially-allocated resources are revoked and status is non-zero.
    fn handle_pipe_create(&mut self, msg: &Message, sender_tid: usize) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(0, [0; 6], 4);

        let creator_pid = self.tid_to_pid.get(&sender_tid).copied().unwrap_or(0);

        // Step 1: create the underlying IPC endpoint.
        let endpoint = match endpoint_create(self.token) {
            Ok(ep) => ep,
            Err(e) => {
                reply_msg.words[0] = e.to_errno() as usize;
                if let Some(tok) = reply_token {
                    let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        };

        // Step 2: derive the write-only (IPC_SEND | GRANT) token.
        // GRANT is required so that the FDAC handler (which calls token_derive
        // on the shell-supplied token) can narrow it further for the child.
        // u64::MAX expiry so the derived child token passes the expiry check.
        let write_token = match token_derive(
            endpoint,
            (Rights::IPC_SEND | Rights::GRANT).bits() as usize,
            u64::MAX,
        ) {
            Ok(t) => t,
            Err(e) => {
                let _ = token_revoke(endpoint);
                reply_msg.words[0] = e.to_errno() as usize;
                if let Some(tok) = reply_token {
                    let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        };

        // Step 3: derive the read-only (IPC_RECV | GRANT) token.
        // GRANT included for the same reason as write_token above.
        let read_token = match token_derive(
            endpoint,
            (Rights::IPC_RECV | Rights::GRANT).bits() as usize,
            u64::MAX,
        ) {
            Ok(t) => t,
            Err(e) => {
                let _ = token_revoke(write_token);
                let _ = token_revoke(endpoint);
                reply_msg.words[0] = e.to_errno() as usize;
                if let Some(tok) = reply_token {
                    let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        };

        // Step 4: allocate a pipe table slot and store the entry.
        let slot_idx = self.allocate_pipe_slot();
        self.pipes[slot_idx] = Some(PipeEntry {
            endpoint,
            creator_pid,
            write_token,
            read_token,
        });
        let pipe_id = Self::pipe_id_encode(slot_idx);

        // Step 5: reply with [status=0, write_token, read_token, pipe_id].
        reply_msg.words[0] = 0;
        reply_msg.words[1] = write_token;
        reply_msg.words[2] = read_token;
        reply_msg.words[3] = pipe_id;
        if let Some(tok) = reply_token {
            let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
        }
        Ok(())
    }

    /// Handle PROCMGR_PIPE_CLOSE_LABEL: revoke and free a pipe.
    ///
    /// words[0] = pipe_id (as returned by PIPE_CREATE).
    ///
    /// Idempotent: if the slot is already empty or out of range, replies
    /// success without doing anything.  Only the creator process may close a
    /// pipe; a non-creator caller gets a silent success (no-op) per spec §4.2.
    ///
    /// On close the kernel tokens are revoked in order: write_token,
    /// read_token, then the root endpoint.  The slot is then set to None.
    ///
    /// Reply: [status=0] always in v1.
    fn handle_pipe_close(&mut self, msg: &Message, sender_tid: usize) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(0, [0; 6], 1);

        let pipe_id = msg.words[0];
        let idx = Self::pipe_id_decode(pipe_id);

        // Steps 1-2: bounds check and presence check (idempotent close).
        if idx >= self.pipes.len() || self.pipes[idx].is_none() {
            reply_msg.words[0] = 0;
            if let Some(tok) = reply_token {
                let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
            }
            return Ok(());
        }

        // Step 3: ownership check — only the creator may close.
        let caller_pid = self.tid_to_pid.get(&sender_tid).copied().unwrap_or(0);
        {
            let entry = self.pipes[idx].as_ref().unwrap();
            if entry.creator_pid != caller_pid {
                // Non-creator: silent success, no revocation.
                reply_msg.words[0] = 0;
                if let Some(tok) = reply_token {
                    let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        }

        // Step 3 (continued): revoke tokens and free the slot.
        let entry = self.pipes[idx].take().unwrap();
        if entry.write_token != 0 {
            let _ = token_revoke(entry.write_token);
        }
        if entry.read_token != 0 {
            let _ = token_revoke(entry.read_token);
        }
        if entry.endpoint != 0 {
            let _ = token_revoke(entry.endpoint);
        }

        // Step 4: reply success.
        reply_msg.words[0] = 0;
        if let Some(tok) = reply_token {
            let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Phase 4 Plan D — job control handlers
    // -------------------------------------------------------------------------

    /// PROCMGR_PG_CREATE_LABEL: allocate a new process group.
    /// Reply: words[0]=0 (ok), words[1]=pgid.
    fn handle_pg_create(&mut self, msg: &Message) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let pgid = self.pg_table.create();
        let mut reply_msg = Message::new(PROCMGR_PG_CREATE_LABEL, [0; 6], 2);
        reply_msg.words[0] = 0;
        reply_msg.words[1] = pgid;
        if let Some(tok) = reply_token {
            let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
        }
        Ok(())
    }

    /// PROCMGR_PG_ATTACH_LABEL: attach pid to pgid.
    /// words[0]=pgid, words[1]=pid. Fire-and-forget.
    fn handle_pg_attach(&mut self, msg: &Message) -> Result<()> {
        let pgid = msg.words[0];
        let pid  = msg.words[1];
        self.pg_table.attach(pgid, pid);
        let reply_token = extract_reply_id(msg);
        if let Some(tok) = reply_token {
            let mut reply_msg = Message::new(PROCMGR_PG_ATTACH_LABEL, [0; 6], 1);
            reply_msg.words[0] = 0;
            let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
        }
        Ok(())
    }

    /// PROCMGR_PG_SUSPEND_LABEL: suspend all pids in a pgid.
    /// words[0]=pgid.
    fn handle_pg_suspend(&mut self, msg: &Message) -> Result<()> {
        let pgid = msg.words[0];
        let pids = self.pg_table.members(pgid);
        for pid in pids {
            if let Some(&cookie) = self.pid_to_cookie.get(&pid) {
                if let Some(&thread_token) = self.exit_table.get(&cookie) {
                    let _ = thread_suspend(thread_token);
                }
            }
            self.pid_stopped.insert(pid);
            // Send job notification to parent.
            let notify_ep = self.exit_notify.get(&self.pid_to_cookie.get(&pid).copied().unwrap_or(0)).copied().unwrap_or(0);
            if notify_ep != 0 {
                let mut jn = Message::new(PROCMGR_JOB_NOTIFY_LABEL, [0; 6], 4);
                jn.words[0] = pgid;
                jn.words[1] = pid;
                jn.words[2] = 1; // Stopped
                jn.words[3] = 0;
                let _ = send(notify_ep, &jn, IpcFlags::empty());
            }
        }
        let reply_token = extract_reply_id(msg);
        if let Some(tok) = reply_token {
            let mut reply_msg = Message::new(PROCMGR_PG_SUSPEND_LABEL, [0; 6], 1);
            reply_msg.words[0] = 0;
            let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
        }
        Ok(())
    }

    /// PROCMGR_PG_RESUME_LABEL: resume all pids in a pgid.
    /// words[0]=pgid.
    fn handle_pg_resume(&mut self, msg: &Message) -> Result<()> {
        let pgid = msg.words[0];
        let pids = self.pg_table.members(pgid);
        for pid in pids {
            if let Some(&cookie) = self.pid_to_cookie.get(&pid) {
                if let Some(&thread_token) = self.exit_table.get(&cookie) {
                    let _ = thread_resume(thread_token);
                }
            }
            self.pid_stopped.remove(&pid);
            // Send job notification to parent.
            let notify_ep = self.exit_notify.get(&self.pid_to_cookie.get(&pid).copied().unwrap_or(0)).copied().unwrap_or(0);
            if notify_ep != 0 {
                let mut jn = Message::new(PROCMGR_JOB_NOTIFY_LABEL, [0; 6], 4);
                jn.words[0] = pgid;
                jn.words[1] = pid;
                jn.words[2] = 2; // Continued
                jn.words[3] = 0;
                let _ = send(notify_ep, &jn, IpcFlags::empty());
            }
        }
        let reply_token = extract_reply_id(msg);
        if let Some(tok) = reply_token {
            let mut reply_msg = Message::new(PROCMGR_PG_RESUME_LABEL, [0; 6], 1);
            reply_msg.words[0] = 0;
            let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
        }
        Ok(())
    }

    /// PROCMGR_PG_SIGNAL_LABEL: send a signal to all pids in a pgid.
    /// words[0]=pgid, words[1]=signum.
    fn handle_pg_signal(&mut self, msg: &Message) -> Result<()> {
        let pgid   = msg.words[0];
        let signum = msg.words[1];
        let pids   = self.pg_table.members(pgid);
        for pid in pids {
            if let Some(&cookie) = self.pid_to_cookie.get(&pid) {
                if let Some(&thread_token) = self.exit_table.get(&cookie) {
                    match signum {
                        s if s == SIGSTOP || s == 17 /* SIGTSTP */ => {
                            let _ = thread_suspend(thread_token);
                            self.pid_stopped.insert(pid);
                            let notify_ep = self.exit_notify.get(&cookie).copied().unwrap_or(0);
                            if notify_ep != 0 {
                                let mut jn = Message::new(PROCMGR_JOB_NOTIFY_LABEL, [0; 6], 4);
                                jn.words[0] = pgid;
                                jn.words[1] = pid;
                                jn.words[2] = 1; // Stopped
                                jn.words[3] = 0;
                                let _ = send(notify_ep, &jn, IpcFlags::empty());
                            }
                        }
                        s if s == SIGCONT => {
                            let _ = thread_resume(thread_token);
                            self.pid_stopped.remove(&pid);
                            let notify_ep = self.exit_notify.get(&cookie).copied().unwrap_or(0);
                            if notify_ep != 0 {
                                let mut jn = Message::new(PROCMGR_JOB_NOTIFY_LABEL, [0; 6], 4);
                                jn.words[0] = pgid;
                                jn.words[1] = pid;
                                jn.words[2] = 2; // Continued
                                jn.words[3] = 0;
                                let _ = send(notify_ep, &jn, IpcFlags::empty());
                            }
                        }
                        s if s == SIGINT || s == SIGTERM || s == SIGKILL => {
                            // Destructive signals: destroy thread, bookkeeping happens
                            // through the normal exit path — just fire the destroy here.
                            let _ = thread_destroy(thread_token);
                        }
                        _ => {
                            // Other signals: log and skip (no async delivery mechanism yet).
                            let _ = debug_print(&format!(
                                "procmgr: pg_signal pgid={} pid={} sig={} (unhandled)",
                                pgid, pid, signum
                            ));
                        }
                    }
                }
            }
        }
        let reply_token = extract_reply_id(msg);
        if let Some(tok) = reply_token {
            let mut reply_msg = Message::new(PROCMGR_PG_SIGNAL_LABEL, [0; 6], 1);
            reply_msg.words[0] = 0;
            let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
        }
        Ok(())
    }

    /// PROCMGR_PID_PGID_QUERY_LABEL: query the pgid of the process owning `tid`.
    /// words[0]=tid. Reply: words[0]=0, words[1]=pgid (0 if not in any group).
    fn handle_pid_pgid_query(&mut self, msg: &Message) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let tid = msg.words[0];
        let pid = self.tid_to_pid.get(&tid).copied().unwrap_or(0);
        let pgid = if pid != 0 {
            self.pg_table.pgid_of(pid).unwrap_or(0)
        } else {
            0
        };
        let mut reply_msg = Message::new(PROCMGR_PID_PGID_QUERY_LABEL, [0; 6], 2);
        reply_msg.words[0] = 0;
        reply_msg.words[1] = pgid;
        if let Some(tok) = reply_token {
            let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // End of Phase 4 Plan D handlers
    // -------------------------------------------------------------------------

    /// Handle PROCMGR_PROC_QUERY_LABEL: /proc file queries from VFS.
    ///
    /// words[0] = query_type (0=status, 1=stat, 2=cmdline, 3=list, 4=comm, 5=exe)
    /// words[1] = target_pid (0 = self, resolved via original_caller_tid)
    /// words[2] = original_caller_tid (forwarded by VFS)
    ///
    /// Reply: words[0]=errno, words[1]=data_len or pid_count, payload=content.
    fn handle_proc_query(&self, msg: &Message, _sender_tid: usize) -> Result<()> {
        let reply_token = match extract_reply_id(msg) {
            Some(t) => t,
            None => return Ok(()),
        };

        let query_type = msg.words[0];
        let target_pid = msg.words[1];
        let original_caller_tid = msg.words[2];

        let mut reply_msg = Message::new(PROCMGR_PROC_QUERY_LABEL, [0; 6], 2);

        // Resolve caller identity for access control.
        let caller_pid = self.tid_to_pid.get(&original_caller_tid).copied().unwrap_or(0);
        let caller_profile = self.pid_to_profile.get(&caller_pid).copied()
            .unwrap_or(CapProfile::empty());
        let session_profile = self.resolve_caller_session(original_caller_tid)
            .map(|s| s.profile)
            .unwrap_or(CapProfile::empty());
        let is_admin = caller_profile.contains(CapProfile::ADMIN)
            || session_profile.contains(CapProfile::ADMIN);

        // Resolve "self" pid.
        let resolved_pid = if target_pid == 0 {
            caller_pid
        } else {
            target_pid
        };

        // Wire format:
        //   words[0] = payload byte length (always; reply_with_payload clobbers
        //              this to payload.len(), so don't use it for errno).
        //   words[1] = errno (0 = success).
        //   words[2] = type-specific count (e.g. pid_count for QUERY_LIST,
        //              content_len for STATUS/STAT/CMDLINE).
        // Older code put errno in words[0] which collided with the payload-len
        // clobber and caused all /proc/* reads through procfs to look like
        // failures (see ps producing zero PID rows pre-fix).

        // Query type 3 (list) doesn't need a specific target.
        if query_type == 3 {
            return self.proc_query_list(reply_token, &mut reply_msg, original_caller_tid, is_admin);
        }

        // For types 0-2, resolved_pid must be valid.
        if resolved_pid == 0 {
            reply_msg.words[1] = Error::NotFound.to_errno() as usize;
            let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            return Ok(());
        }

        // Access control: caller must be admin OR target must be in same session.
        if !is_admin {
            let caller_session_cid = self.resolve_caller_session(original_caller_tid)
                .map(|s| s.container_id)
                .unwrap_or(0);
            let target_session_cid = self.pid_to_container_id.get(&resolved_pid)
                .and_then(|&cid| {
                    let mut walk = cid;
                    while walk != 0 {
                        if self.session_table.contains_key(&walk) {
                            return Some(walk);
                        }
                        match self.container_instances.get(&walk) {
                            Some(inst) => walk = inst.parent_container_id,
                            None => break,
                        }
                    }
                    None
                })
                .unwrap_or(0);
            if caller_session_cid == 0 || caller_session_cid != target_session_cid {
                reply_msg.words[1] = Error::PermissionDenied.to_errno() as usize;
                let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                return Ok(());
            }
        }

        // Look up container instance for the target pid.
        let cid = self.pid_to_container_id.get(&resolved_pid).copied().unwrap_or(0);
        let inst = if cid != 0 { self.container_instances.get(&cid) } else { None };

        match query_type {
            0 => self.proc_query_status(reply_token, &mut reply_msg, resolved_pid, inst),
            1 => self.proc_query_stat(reply_token, &mut reply_msg, resolved_pid, inst),
            2 => self.proc_query_cmdline(reply_token, &mut reply_msg, inst),
            4 => self.proc_query_comm(reply_token, &mut reply_msg, inst),
            5 => self.proc_query_exe(reply_token, &mut reply_msg, inst),
            _ => {
                reply_msg.words[1] = Error::InvalidArgument.to_errno() as usize;
                let _ = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                Ok(())
            }
        }
    }

    /// /proc/<pid>/status: human-readable key-value status.
    fn proc_query_status(
        &self,
        reply_token: usize,
        reply_msg: &mut Message,
        pid: usize,
        inst: Option<&ContainerInstance>,
    ) -> Result<()> {
        let name = inst.map(|i| i.name.as_str()).unwrap_or("?");
        let state = if self.pid_to_tid.get(&pid).is_some() { "R" } else { "Z" };
        let profile = self.pid_to_profile.get(&pid).copied()
            .unwrap_or(CapProfile::empty());
        let session_id = inst.map(|i| i.session_id).unwrap_or(0);
        let cid = inst.map(|i| i.container_id).unwrap_or(0);

        // Resolve VT via session parent chain.
        let vt = inst.and_then(|i| {
            let mut walk = i.parent_container_id;
            while walk != 0 {
                if let Some(session) = self.session_table.get(&walk) {
                    return Some(session.vt_index);
                }
                match self.container_instances.get(&walk) {
                    Some(p) => walk = p.parent_container_id,
                    None => break,
                }
            }
            None
        }).unwrap_or(0xFF);

        let content = format!(
            "Name:\t{}\nPid:\t{}\nState:\t{}\nProfile:\t{:#x}\nSession:\t{}\nVt:\t{}\nContainerId:\t{}\n",
            name, pid, state, profile.bits(), session_id, vt, cid
        );

        reply_msg.words[1] = 0; // errno success
        reply_msg.words[2] = content.len();
        let _ = ipc::reply_with_payload(reply_token, reply_msg, content.as_bytes());
        Ok(())
    }

    /// /proc/<pid>/stat: single-line stat for ps-style output.
    fn proc_query_stat(
        &self,
        reply_token: usize,
        reply_msg: &mut Message,
        pid: usize,
        inst: Option<&ContainerInstance>,
    ) -> Result<()> {
        let name = inst.map(|i| i.name.as_str()).unwrap_or("?");
        let state_char = if self.pid_to_tid.get(&pid).is_some() { 'R' } else { 'Z' };

        // Fetch kernel stats if available.
        let (cpu_ticks, heap_pages, other_pages): (u64, u16, u16) = if let Some(&cookie) = self.pid_to_cookie.get(&pid) {
            let ticks = self.exit_table.get(&cookie)
                .map(|&tt| libcluu::syscall::thread_get_stats(tt).unwrap_or(0))
                .unwrap_or(0);
            let (code, heap, stack) = self.cookie_to_space.get(&cookie)
                .map(|&sp| libcluu::syscall::space_get_stats(sp).unwrap_or((0, 0, 0)))
                .unwrap_or((0, 0, 0));
            (ticks, heap, code.saturating_add(stack))
        } else {
            (0, 0, 0)
        };

        // CLUU /proc/<pid>/stat extension: include parent_pid (resolved via
        // parent_container_id → pid), session_id, container_id, parent_cid.
        // Format:
        //   pid (name) state cpu_ticks heap_pages other_pages ppid sid cid pcid
        // ppid is 0 if there is no parent container in the table, or no pid
        // is currently associated with the parent.
        let (sid, cid, pcid) = match inst {
            Some(i) => (i.session_id, i.container_id, i.parent_container_id),
            None => (0, 0, 0),
        };
        let ppid = if pcid != 0 {
            self.pid_to_container_id
                .iter()
                .find_map(|(&p, &c)| if c == pcid { Some(p) } else { None })
                .unwrap_or(0)
        } else {
            0
        };

        let content = format!(
            "{} ({}) {} {} {} {} {} {} {} {}\n",
            pid, name, state_char, cpu_ticks, heap_pages, other_pages,
            ppid, sid, cid, pcid
        );

        reply_msg.words[1] = 0; // errno success
        reply_msg.words[2] = content.len();
        let _ = ipc::reply_with_payload(reply_token, reply_msg, content.as_bytes());
        Ok(())
    }

    /// /proc/<pid>/cmdline: NUL-terminated image path.
    fn proc_query_cmdline(
        &self,
        reply_token: usize,
        reply_msg: &mut Message,
        inst: Option<&ContainerInstance>,
    ) -> Result<()> {
        let path = inst.map(|i| i.image_path.as_str()).unwrap_or("");
        let mut content = Vec::with_capacity(path.len() + 1);
        content.extend_from_slice(path.as_bytes());
        content.push(0);

        reply_msg.words[1] = 0; // errno success
        reply_msg.words[2] = content.len();
        let _ = ipc::reply_with_payload(reply_token, reply_msg, &content);
        Ok(())
    }

    /// /proc/<pid>/comm: short process name, newline-terminated, capped at the
    /// Linux convention of TASK_COMM_LEN-1 = 15 bytes of name plus '\n'.
    fn proc_query_comm(
        &self,
        reply_token: usize,
        reply_msg: &mut Message,
        inst: Option<&ContainerInstance>,
    ) -> Result<()> {
        const COMM_MAX: usize = 15;
        let name = inst.map(|i| i.name.as_str()).unwrap_or("");
        let trunc_end = name.char_indices()
            .take_while(|&(idx, ch)| idx + ch.len_utf8() <= COMM_MAX)
            .last()
            .map(|(idx, ch)| idx + ch.len_utf8())
            .unwrap_or(0);
        let trimmed = &name[..trunc_end];
        let mut content = Vec::with_capacity(trimmed.len() + 1);
        content.extend_from_slice(trimmed.as_bytes());
        content.push(b'\n');

        reply_msg.words[1] = 0;
        reply_msg.words[2] = content.len();
        let _ = ipc::reply_with_payload(reply_token, reply_msg, &content);
        Ok(())
    }

    /// /proc/<pid>/exe: image path of the running binary, newline-terminated.
    /// Linux exposes this as a symlink; we return the resolved path as plain text.
    fn proc_query_exe(
        &self,
        reply_token: usize,
        reply_msg: &mut Message,
        inst: Option<&ContainerInstance>,
    ) -> Result<()> {
        let path = inst.map(|i| i.image_path.as_str()).unwrap_or("");
        let mut content = Vec::with_capacity(path.len() + 1);
        content.extend_from_slice(path.as_bytes());
        content.push(b'\n');

        reply_msg.words[1] = 0;
        reply_msg.words[2] = content.len();
        let _ = ipc::reply_with_payload(reply_token, reply_msg, &content);
        Ok(())
    }

    /// /proc list: packed u32 LE pid array of visible processes.
    fn proc_query_list(
        &self,
        reply_token: usize,
        reply_msg: &mut Message,
        original_caller_tid: usize,
        is_admin: bool,
    ) -> Result<()> {
        let caller_session_cid = if !is_admin {
            self.resolve_caller_session(original_caller_tid)
                .map(|s| s.container_id)
                .unwrap_or(0)
        } else {
            0
        };

        let mut pids: Vec<u8> = Vec::new();
        let mut pid_count: usize = 0;

        for (&pid, &cid) in &self.pid_to_container_id {
            if !is_admin {
                // Walk up parent chain to check session membership.
                let mut walk = cid;
                let mut in_session = false;
                while walk != 0 {
                    if walk == caller_session_cid {
                        in_session = true;
                        break;
                    }
                    match self.container_instances.get(&walk) {
                        Some(inst) => walk = inst.parent_container_id,
                        None => break,
                    }
                }
                if !in_session { continue; }
            }
            pids.extend_from_slice(&(pid as u32).to_le_bytes());
            pid_count += 1;
        }

        reply_msg.words[1] = 0; // errno success
        reply_msg.words[2] = pid_count;
        let _ = ipc::reply_with_payload(reply_token, reply_msg, &pids);
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
        if param_count > 12 {
            let _ = debug_print("procmgr: service spawn rejected: too many params");
            return Ok(());
        }

        // Parse path from payload — copy to owned String immediately so that
        // subsequent outbound IPC calls (VFS open, map_elf, etc.) cannot trash
        // the borrowed slice that `parse_cstr` points into.
        let path = match parse_cstr(payload) {
            Some(p) => alloc::string::String::from(p),
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
        let mut params = [0u64; 16];
        for i in 0..param_count {
            let offset = i * 10; // 2 bytes index + 8 bytes value
            if offset + 10 > param_data.len() {
                let _ = debug_print("procmgr: service spawn: param data truncated");
                break;
            }
            let idx = u16::from_le_bytes([param_data[offset], param_data[offset + 1]]) as usize;

            // ── Policy: validate param index bounds ──
            if idx >= 16 {
                let _ = debug_print(&format!(
                    "procmgr: service spawn rejected: param index {} out of range",
                    idx
                ));
                return Ok(());
            }
            // ── Policy: slots 10/11 (PARAM_CWD_OFFSET/LEN), 12/13 (PARAM_REDIR_OFFSET/LEN),
            // and 14/15 (PARAM_FD_VFS_OFFSET/LEN) are procmgr-trusted. They are written by
            // procmgr itself; external callers must not forge these metadata slots.
            if idx == PARAM_CWD_OFFSET || idx == PARAM_CWD_LEN
                || idx == PARAM_REDIR_OFFSET || idx == PARAM_REDIR_LEN
                || idx == PARAM_FD_VFS_OFFSET || idx == PARAM_FD_VFS_LEN
            {
                let _ = debug_print(&format!(
                    "procmgr: service spawn rejected: reserved metadata slot {}",
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
        let service_bytes = find_member(initrd, &path).ok_or(Error::NotFound)?;
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
            THREAD_CREATE_START_SUSPENDED,
        )?;

        // Register VFS view for the service based on its profile.
        // System services (pid=0) don't get private storage — container_id=0.
        // parent_container_id=0, is_identity_switch=false: procmgr is spawner;
        // no parent to check against.
        let view_mounts = default_view_for_profile(requested_profile);
        self.install_view_and_run(thread_token, &view_mounts, requested_profile, 0, 0, false);

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

        // Copy path to owned String immediately — subsequent outbound IPC calls
        // (VFS open, map_elf, etc.) reuse the same syscall buffer that payload
        // borrows from, so any &str/&[u8] derived from payload must be owned
        // before the first IPC call.
        let path = match parse_cstr(payload) {
            Some(value) => alloc::string::String::from(value),
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

        let mut priority = DEFAULT_PRIORITY;

        // Enforce container quotas
        if let Some(container) = self.container_instances.get(&caller_container_id) {
            if let Some(max) = container.quota.max_processes {
                if container.live_processes >= max {
                    let _ = debug_print(&format!(
                        "procmgr: spawn denied — container {} at process limit ({})",
                        caller_container_id, max
                    ));
                    reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                    let _ = self.send_spawn_reply(reply_token, &reply_msg);
                    return Ok(());
                }
            }
            if let Some(max_p) = container.quota.max_priority {
                priority = priority.min(max_p as usize);
            }
        }

        // Extract argv data: payload is [path\0, argv[0]\0, argv[1]\0, ...]
        let argc = if msg.tag.words >= 2 { msg.words[1] } else { 0 };
        let fdac_offset = if msg.tag.words >= 3 { msg.words[2] } else { 0 };

        // Strip the CWD trailer first so argv/fdac slices don't extend into it.
        // Own all slices before any outbound IPC (argv_data, fdac_data, cwd_bytes
        // all point into the kernel recv buffer which VFS calls will overwrite).
        let (effective_payload, cwd_bytes_ref) = split_cwd_trailer(payload);

        let path_nul_end = effective_payload
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(effective_payload.len())
            + 1;
        let argv_data: alloc::vec::Vec<u8> = if argc > 0 && path_nul_end < effective_payload.len() {
            effective_payload[path_nul_end..].to_vec()
        } else {
            alloc::vec::Vec::new()
        };
        let fdac_data: alloc::vec::Vec<u8> = if fdac_offset > 0 && fdac_offset < effective_payload.len() {
            effective_payload[fdac_offset..].to_vec()
        } else {
            alloc::vec::Vec::new()
        };
        let cwd_bytes: alloc::vec::Vec<u8> = cwd_bytes_ref.to_vec();

        match self.spawn_service_with_env(
            &path,
            priority,
            &argv_data,
            argc,
            &[],
            0,
            sender_tid,
            spawn_seq,
            spawn_start,
            &fdac_data,
            child_profile,
            0,
            0,
            &[],
            Some(&child_view_mounts),
            &cwd_bytes,
            &[], // no redir for posix_spawn path
            THREAD_CREATE_START_SUSPENDED,
        ) {
            Ok((thread_token, cookie, pid, child_stdin_send)) => {
                reply_msg.words[0] = 0;
                reply_msg.words[1] = pid;
                reply_msg.words[2] = cookie;
                reply_msg.words[3] = child_stdin_send;
                // Inherit parent's container.
                self.pid_to_container_id.insert(pid, caller_container_id);
                if let Some(container) = self.container_instances.get_mut(&caller_container_id) {
                    container.live_processes = container.live_processes.saturating_add(1);
                }
                // parent_container_id=caller_container_id: POSIX spawn child inherits the
                // caller's container and view.  Assert the cloned view is not wider than
                // the parent's recorded view (it should always be equal — but check).
                self.install_view_and_run(thread_token, &child_view_mounts, child_profile, caller_container_id, caller_container_id, false);
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
        // Remove from job-control state if the process was stopped.
        self.pid_stopped.remove(&pid);
        // Detach from any process group it belonged to.
        if let Some(pgid) = self.pg_table.pgid_of(pid) {
            self.pg_table.detach(pgid, pid);
        }
        if let Some(thread_tid) = self.pid_to_tid.remove(&pid) {
            self.tid_to_pid.remove(&thread_tid);
            self.broadcast_blk_tid_cleanup(thread_tid);
        }
        for idx in 0..self.pipes.len() {
            if self.pipes[idx].as_ref().map_or(false, |e| e.creator_pid == pid) {
                let entry = self.pipes[idx].take().unwrap();
                if entry.write_token != 0 {
                    let _ = token_revoke(entry.write_token);
                }
                if entry.read_token != 0 {
                    let _ = token_revoke(entry.read_token);
                }
                let _ = token_revoke(entry.endpoint);
            }
        }
    }

    /// Tell virtio-blk that `tid` exited so it can reap any open sessions.
    /// Resolves the blkdev endpoint lazily — boots before blkdev would fail.
    fn broadcast_blk_tid_cleanup(&mut self, tid: usize) {
        if self.blkdev_endpoint == 0 {
            self.blkdev_endpoint =
                registry::subscribe_output("blkdev", "main").unwrap_or(0);
        }
        if self.blkdev_endpoint == 0 {
            return;
        }
        let msg = Message::new(libcluu::ipc::BLK_TID_CLEANUP, [tid, 0, 0, 0, 0, 0], 1);
        let _ = send(self.blkdev_endpoint, &msg, IpcFlags::empty());
    }

    #[allow(clippy::too_many_arguments, dead_code)]
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
            &[],
            &[], // no redir
            0,
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
        cwd_bytes: &[u8],
        redir_bytes: &[u8],
        thread_flags: usize,
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
        let space_token = space_create(self.token)?;
        self.log_spawn_stage(spawn_seq, "space_create_done", spawn_start);

        let mut entry_point = 0usize;
        let mut mapped = false;

        if !path.starts_with("/dev/initrd/") {
            self.log_spawn_stage(spawn_seq, "elf_fetch_start", spawn_start);
            match self.map_elf_from_vfs(path, space_token, caller_view) {
                Ok(Some(entry)) => {
                    entry_point = entry;
                    mapped = true;
                    self.log_spawn_stage(spawn_seq, "elf_fetch_done", spawn_start);
                }
                Ok(None) => {
                    let _ = debug_print(&format!("procmgr: map_elf_from_vfs returned None for {}", path));
                }
                Err(ref e) => {
                    let _ = debug_print(&format!("procmgr: map_elf_from_vfs error for {} {:?}", path, e));
                }
            }
        }

        if !mapped {
            // Fall back to loading bytes in-process.
            self.log_spawn_stage(spawn_seq, "elf_fetch_start", spawn_start);
            let (elf_data, _from_vfs) = self.load_elf(path, caller_view)?;
            let service_bytes: &[u8] = &elf_data;

            let elf = ElfFile::parse(service_bytes)?;
            entry_point = elf.entry_point as usize;
            libcluu::map_segments(space_token, &elf, service_bytes)?;
            self.log_spawn_stage(spawn_seq, "elf_fetch_done", spawn_start);
            self.log_spawn_stage(spawn_seq, "map_segments_done", spawn_start);
        } else {
            self.log_spawn_stage(spawn_seq, "map_segments_done", spawn_start);
        }

        libcluu::map_stack(
            space_token,
            SERVICE_STACK_TOP,
            SERVICE_STACK_SIZE,
            STACK_FLAGS,
        )?;
        self.log_spawn_stage(spawn_seq, "stack_map_done", spawn_start);

        let send_rights = Rights::IPC_SEND.bits() as usize;
        let child_endpoint = token_derive(self.exit_endpoint, send_rights, u64::MAX)?;
        let cookie = self.next_exit_cookie();
        let pid = self.next_pid();
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

        // Create the child thread SUSPENDED before FDAC parsing so that the child's TID
        // is available for VFS derive_child_fd (which registers the fd slot under child_tid).
        // The thread stays suspended until install_view_and_run → thread_resume.
        let thread_token = thread_create(space_token, entry_point, SERVICE_STACK_TOP, priority, thread_flags)?;
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

        // Parse FDAC (fd actions) to override child stdio endpoints.
        // fd_vfs_meta[i] = (child_vfs_client_id, child_vfs_remote_fd) for fd i.
        // Set to (0, 0) for legacy (tty/pipe) fds; non-zero means VFS-backed.
        let mut pipe_mask: u8 = 0;
        let mut stdin_ep = stdin_endpoint;
        let mut stdout_ep = stdout_endpoint;
        let mut stderr_ep = stderr_endpoint;
        let mut stdlog_ep = stdlog_endpoint;
        let mut fd_vfs_meta: [(usize, usize); 4] = [(0, 0); 4];

        if fdac_data.len() >= 8 {
            let magic =
                u32::from_le_bytes([fdac_data[0], fdac_data[1], fdac_data[2], fdac_data[3]]);
            let count = u32::from_le_bytes([fdac_data[4], fdac_data[5], fdac_data[6], fdac_data[7]])
                as usize;
            if magic == 0x46444143 && count <= 4 {
                // Each FdAction is 32 bytes:
                //   bytes  0– 3: u32 target_fd
                //   bytes  4– 7: u32 flags
                //   bytes  8–15: usize endpoint  (legacy: pipes/tty)
                //   bytes 16–23: usize vfs_client_id (0 = not VFS-backed)
                //   bytes 24–31: usize vfs_remote_fd  (0 = not VFS-backed)
                for i in 0..count {
                    let base = 8 + i * 32;
                    if base + 32 > fdac_data.len() {
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
                    let vfs_client_id = usize::from_le_bytes([
                        fdac_data[base + 16],
                        fdac_data[base + 17],
                        fdac_data[base + 18],
                        fdac_data[base + 19],
                        fdac_data[base + 20],
                        fdac_data[base + 21],
                        fdac_data[base + 22],
                        fdac_data[base + 23],
                    ]);
                    let vfs_remote_fd = usize::from_le_bytes([
                        fdac_data[base + 24],
                        fdac_data[base + 25],
                        fdac_data[base + 26],
                        fdac_data[base + 27],
                        fdac_data[base + 28],
                        fdac_data[base + 29],
                        fdac_data[base + 30],
                        fdac_data[base + 31],
                    ]);

                    let is_pipe = (flags & 0x01) != 0;
                    // Validate + narrow: stdin needs recv, others need send.
                    // IPC_CALL is added for stdout/stderr so that read_tty()
                    // (which sends TTY_READ_REQUEST_LABEL as a synchronous call
                    // via fd 1's endpoint) can complete successfully.
                    // GRANT is included so that descendants (login → shell) can
                    // re-derive via FDAC.  Mirrors the pre-existing pipe-endpoint
                    // pattern at main.rs:2901-2924.
                    let probe_rights = match target_fd {
                        0 => (Rights::IPC_RECV | Rights::GRANT).bits() as usize,
                        _ => (Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT).bits() as usize,
                    };

                    // Branch: VFS-backed fd → ask VFS to mint a child token from its own endpoint.
                    // Legacy path (pipes, tty) → direct token_derive as before.
                    let (derived, child_cid, child_rfd) = if vfs_remote_fd != 0 {
                        // Ensure we have a VFS endpoint before deriving (lazy init).
                        if self.vfs_endpoint == 0 {
                            let _ = self.ensure_vfs_endpoint();
                        }
                        // VFS keys self.files by authenticated sender_tid. libcluu stored
                        // registry::control_endpoint() as `vfs_client_id` (FdEntry::client_id),
                        // but VFS authenticates by kernel-supplied sender_tid. Override here.
                        let parent_cid_for_vfs = if owner_tid != 0 { owner_tid } else { vfs_client_id };
                        // VFS-backed fd: procmgr asks VFS to clone the open file to the child.
                        // child_tid is the new client_id for the child in VFS — this is the TID
                        // the kernel will report as sender_tid when the child sends VFS requests.
                        match vfs_derive_child_fd(
                            self.vfs_endpoint,
                            parent_cid_for_vfs,
                            vfs_remote_fd,
                            probe_rights,
                            thread_tid,
                        ) {
                            Ok((tok, cid, rfd)) => (tok, cid, rfd),
                            Err(e) => {
                                let _ = debug_print(&format!(
                                    "procmgr: FDAC vfs_derive_child_fd failed fd={} parent_cid={} rfd={} err={:?}",
                                    target_fd, parent_cid_for_vfs, vfs_remote_fd, e
                                ));
                                return Err(Error::PermissionDenied);
                            }
                        }
                    } else {
                        // Legacy path: direct token_derive (pipes, tty endpoints).
                        match token_derive(endpoint, probe_rights, u64::MAX) {
                            Ok(derived) => (derived, 0usize, 0usize),
                            Err(_) => {
                                let _ = debug_print(&format!(
                                    "procmgr: FDAC rejected: endpoint {} for fd {} failed derive",
                                    endpoint, target_fd
                                ));
                                return Err(Error::PermissionDenied);
                            }
                        }
                    };

                    // Stash VFS metadata (non-zero only for VFS-backed fds).
                    if target_fd < 4 {
                        fd_vfs_meta[target_fd as usize] = (child_cid, child_rfd);
                    }

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
                let _ = debug_print(&format!(
                    "procmgr: FDAC parsed {} actions, pipe_mask=0x{:02x}",
                    count, pipe_mask
                ));
            }
        }

        // TTY needs CALL (in addition to SEND) so it can do synchronous
        // tab-completion queries to the shell over the same endpoint.
        let parent_stdin_send = match token_derive(
            stdin_ep,
            (Rights::IPC_SEND.bits() | Rights::IPC_CALL.bits()) as usize,
            u64::MAX,
        ) {
            Ok(token) => token,
            Err(_) => 0,
        };

        // Apply caller-specified param overrides (console instance/active, etc.).
        let mut eo_buf = [(0usize, 0u64); 12];
        let mut n_eo = 0usize;
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
            cwd_bytes,
            redir_bytes,
            &fd_vfs_meta,
        )?;

        // thread_token and thread_tid were obtained above (before FDAC) so the
        // child TID could be used as the VFS client_id during derive_child_fd.
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

        // Open the file
        let file = match client.open(path) {
            Ok(f) => f,
            Err(e) => {
                let _ = debug_print(&format!("procmgr: VFS open '{}' failed {:?}", path, e));
                return None;
            }
        };

        if file.size == 0 {
            let _ = debug_print(&format!("procmgr: load_from_vfs size=0 for {}", path));
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
                let _ = debug_print("procmgr: space_map_range FAILED");
                let _ = client.close(file);
                return None;
            }
        }

        let mut data = Vec::with_capacity(file.size);

        // Read file in chunks (optionally priming cache with a full read request).
        let mut offset = 0;
        if use_full_read {
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
        }

        while offset < file.size {
            let remaining = file.size - offset;
            let read_size = remaining.min(CHUNK_SIZE);

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
                }
                Err(e) => {
                    let _ = debug_print(&format!("procmgr: read_grant FAILED {:?}", e));
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

        // Persistent ring: allocate region once and reuse across loads.
        // Per-call alloc/free was creating fresh phys pages each time, which
        // unbinds VFS's space_grant — the magic VFS writes is invisible
        // here and SharedRing::attach fails with InvalidState.
        if self.vfs_read_ring.is_none() {
            match libcluu::ipc::alloc_shared_ring_region(
                self.space_token,
                RING_BYTES,
                libcluu::ipc::SHARED_RING_DEFAULT_MAP_FLAGS,
            ) {
                Ok(region) => self.vfs_read_ring = Some(region),
                Err(err) => {
                    let _ = debug_print(&format!("procmgr: ring alloc failed {:?}", err));
                    return None;
                }
            }
        }
        let region = self.vfs_read_ring.as_ref().unwrap();

        let ring_meta = match client.setup_read_ring(self.space_token, region.base, region.bytes) {
            Ok(meta) => meta,
            Err(err) => {
                let _ = debug_print(&format!("procmgr: ring setup failed {:?}", err));
                return None;
            }
        };

        if ring_meta.bytes > region.bytes {
            let _ = debug_print("procmgr: ring invalid bytes from vfs");
            return None;
        }

        let backing =
            unsafe { core::slice::from_raw_parts_mut(region.base as *mut u8, ring_meta.bytes) };
        let mut ring = match SharedRing::attach(backing) {
            Ok(ring) => ring,
            Err(err) => {
                let _ = debug_print(&format!("procmgr: ring attach failed {:?}", err));
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
                return None;
            }

            offset += popped;
            if chunk.eof {
                break;
            }
        }

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
            Err(e) => {
                let _ = debug_print(&format!("procmgr: cached_vfs_file failed path={} err={:?}", effective_path, e));
                return Ok(None);
            }
        };
        let _ = debug_print(&format!("procmgr: map_elf attempt path={} fd={}", effective_path, file.fd));
        let map_token = match token_derive(space_token, Rights::SPACE_MAP.bits() as usize, u64::MAX) {
            Ok(t) => t,
            Err(e) => {
                let _ = debug_print(&format!("procmgr: map_elf token_derive failed {:?}", e));
                return Err(e);
            }
        };
        let first_attempt = client.map_elf(file, map_token);
        let entry = match first_attempt {
            Ok(entry) => Some(entry),
            Err(err) => {
                let _ = debug_print(&format!("procmgr: map_elf failed {:?}", err));
                // Likely stale fd or VFS-side eviction: refresh once and retry.
                // Bypass the cache on retry — re-entering cached_vfs_file on
                // the error path produced a procmgr null-deref (CR2=0x298,
                // RIP=0x494d00 in vfs_file_cache.len) once seen 2026-05-09.
                self.invalidate_cached_vfs_file(&client, effective_path);
                match client.open(effective_path) {
                    Ok(refreshed_file) => {
                        let _ = debug_print(&format!("procmgr: map_elf retry fd={}", refreshed_file.fd));
                        match client.map_elf(refreshed_file, map_token) {
                            Ok(e) => Some(e),
                            Err(err) => {
                                let _ = debug_print(&format!("procmgr: map_elf retry failed {:?}", err));
                                None
                            }
                        }
                    }
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

        // Strip trailers from the kernel recv buffer.  All derived slices are
        // owned (Vec<u8>) immediately so that subsequent VFS IPC calls cannot
        // trash the buffer they point into before spawn_service_with_env
        // consumes them.
        let (effective_payload, cwd_bytes_ref) = split_cwd_trailer(payload);
        let (effective_payload, env_bytes_ref) = split_env_trailer(effective_payload);
        let (effective_payload, redir_bytes_ref) = split_redir_trailer(effective_payload);
        let (effective_payload, argv_extra_bytes_ref) = split_argv_trailer(effective_payload);

        let cwd_bytes: alloc::vec::Vec<u8> = cwd_bytes_ref.to_vec();
        let env_bytes: alloc::vec::Vec<u8> = env_bytes_ref.to_vec();
        let redir_bytes: alloc::vec::Vec<u8> = redir_bytes_ref.to_vec();
        let argv_extra_bytes: alloc::vec::Vec<u8> = argv_extra_bytes_ref.to_vec();

        // Count env entries (one NUL per "KEY=VALUE\0" record).
        let envc = env_bytes.iter().filter(|&&b| b == 0).count();

        // Extract FDAC offset and param override info from message words
        let fdac_offset = if msg.tag.words >= 3 { msg.words[2] } else { 0 };
        let param_offset = if msg.tag.words >= 4 { msg.words[3] } else { 0 };
        let param_count = if msg.tag.words >= 5 { msg.words[4] } else { 0 };

        // Extract image name from payload (NUL-terminated, bounded by FDAC or param offset)
        let name_end = if fdac_offset > 0 && fdac_offset <= effective_payload.len() {
            fdac_offset
        } else if param_offset > 0 && param_offset <= effective_payload.len() {
            param_offset
        } else {
            effective_payload.len()
        };
        // Own the image_name String immediately — VFS calls (read_file_from_vfs,
        // spawn_service_with_env → map_elf_from_vfs) overwrite the kernel recv
        // buffer that effective_payload borrows from.
        let image_name: alloc::string::String = match core::str::from_utf8(&effective_payload[..name_end]) {
            Ok(s) => alloc::string::String::from(s.trim_end_matches('\0').trim()),
            Err(_) => {
                let _ = debug_print(&format!(
                    "procmgr: container_run rejected: payload not UTF-8 (len={} name_end={})",
                    effective_payload.len(), name_end
                ));
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // Extract FDAC data from payload (after image name, before trailer).
        // Own the slice so the IPC buffer reuse does not corrupt it.
        let fdac_data: alloc::vec::Vec<u8> = if fdac_offset > 0 && fdac_offset < effective_payload.len() {
            effective_payload[fdac_offset..].to_vec()
        } else {
            alloc::vec::Vec::new()
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
        if image_name.contains('/') {
            let _ = debug_print(&format!(
                "procmgr: container_run rejected: image name '{}' contains '/' (use bare name; resolve symlinks shell-side)",
                image_name
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

        // Parse restart policy from [lifecycle] section
        let restart_policy = parse_restart_policy(&doc);

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
        // [[mounts.policy]] — per-path inheritance policy, applied on top of
        // defaults. Parsed with a raw-text fallback because libcluu::toml does
        // not yet expose array-of-tables. Consumed below by
        // `resolve_effective_policies` in the view-building block.
        let cluufile_mount_policies: Vec<MountPolicyEntry> =
            parse_mount_policies_raw(manifest_str);

        // UE13: strict Cluufile-vs-parent validation. If the caller already
        // has a recorded view, every Cluufile MOUNT directive must be
        // satisfiable by that view. A bare boot/autostart caller (no stored
        // view) is exempted — the supervisor envelope is implicit there.
        if sender_tid != 0 {
            if let Some(parent_pid) = self.tid_to_pid.get(&sender_tid).copied() {
                if let Some(parent_view) = self.pid_to_view.get(&parent_pid) {
                    if let Err(reason) = validate_cluufile_against_parent(
                        &cluufile_mount_policies,
                        parent_view.as_slice(),
                    ) {
                        let _ = debug_print(&format!(
                            "procmgr: cluufile mismatch: {}",
                            reason
                        ));
                        reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
                        if let Some(tok) = reply_token {
                            let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
                        }
                        return Ok(());
                    }
                }
            }
        }

        if has_persistent_storage {
            if !self.create_container_dirs(container_id, &image_name) {
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

        // Build argv payload with binary path as argv[0], followed by user argv
        // bytes extracted from the ARGV trailer.
        let mut argv_payload: Vec<u8> = Vec::new();
        argv_payload.extend_from_slice(binary.as_bytes());
        argv_payload.push(0);
        let mut argc = 1usize;

        // argv_extra_bytes is a concatenation of NUL-terminated argv[0..] strings
        // from the shell. Append verbatim and count NULs to derive argc.
        if !argv_extra_bytes.is_empty() {
            argv_payload.extend_from_slice(&argv_extra_bytes);
            argc += argv_extra_bytes.iter().filter(|&&b| b == 0).count();
        }

        // Parse param overrides from payload (G1+G2: wire format extension)
        let mut param_overrides_buf = [(0usize, 0u64); 10];
        let mut n_overrides = 0;
        if param_count > 0 && param_offset > 0 && param_offset < effective_payload.len() {
            let param_data = &effective_payload[param_offset..];
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
            &env_bytes,
            envc,
            sender_tid,
            spawn_seq,
            spawn_start,
            &fdac_data,
            requested_profile,
            extra_token,
            extra_token_1,
            param_overrides,
            None, // no caller view (container run uses absolute /var/images/ paths)
            &cwd_bytes,
            &redir_bytes,
            THREAD_CREATE_START_SUSPENDED,
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
                    let filtered: ViewMountList = caller_view.into_iter()
                        .filter(|(_, dst, _, _)| {
                            !deny_paths.iter().any(|deny| dst == deny || dst.starts_with(&format!("{}/", deny)))
                        })
                        .collect();

                    filtered
                } else if deny_inherit {
                    // deny_inherit = true: ONLY image dirs + container storage, no passthrough
                    let mut mounts = ViewMountList::new();
                    for dir in &image_dirs {
                        mounts.push((
                            format!("/var/images/{}/{}", image_name, dir),
                            format!("/{}", dir),
                            false, // read-only
                            0u64,
                        ));
                    }
                    mounts
                } else {
                    // Top-level: default view (current behavior)
                    default_view_for_profile(requested_profile)
                };

                // Resolve effective mount policies (defaults + Cluufile overrides).
                let effective_policies = resolve_effective_policies(
                    &cluufile_mount_policies,
                    deny_inherit,
                );

                // Strip /tmp, /log, /data inherited from the caller view —
                // procmgr re-applies those with the right per-container
                // MemFs backend below (private/inherit/ro per Cluufile).
                //
                // `/` is NOT stripped: if the envelope grants the caller
                // `rw:/` (admin/supervisor) we want children to inherit that
                // ext2 passthrough so `edit /hello.txt` actually persists.
                // The catchall `/ → MemFs{own_cid}` we append below sits at
                // the end of the view list and only fires when no earlier
                // mount covers the path (first-match-wins) — so an inherited
                // `rw:/` shadows it without any extra logic.
                let container_anchored = ["/tmp", "/log", "/data"];
                view_mounts.retain(|(_, dst, _, _)| {
                    !container_anchored.iter().any(|a| dst == *a)
                });

                // Prepend policy-driven /tmp and /log mounts with the right
                // memfs_cid (first-match-wins — these shadow any leftover
                // passthrough entries that slipped through retain above).
                let memfs_mounts = policy_driven_memfs_mounts(
                    &effective_policies,
                    container_id,
                    caller_container_id,
                );
                for m in memfs_mounts.into_iter().rev() {
                    view_mounts.insert(0, m);
                }

                // /data is a per-container system mount regardless of policy;
                // prepend so it shadows any passthrough /data.
                for m in container_system_mounts(container_id).into_iter().rev() {
                    view_mounts.insert(0, m);
                }

                // The / catch-all (MemFs{own_cid}) is APPENDED so it only
                // matches paths no more specific mount covered. VFS resolves
                // first-match-wins, so the catch-all must come last.
                view_mounts.extend(container_catchall_mount(container_id));

                // PERSISTENT directives already contribute to view_mounts via
                // the existing storage-table loop below (preserve that path).
                if has_persistent_storage && container_id > 0 {
                    if let Some(storage_table) = doc.table("storage") {
                        if let Some(pdirs) = storage_table.get_array("persistent_dirs") {
                            for pdir in pdirs {
                                let dir_name = pdir.trim_start_matches('/');
                                if let Some(pos) = view_mounts.iter().position(|(_, dst, _, _)| dst.trim_start_matches('/') == dir_name) {
                                    view_mounts.remove(pos);
                                }
                                view_mounts.insert(0, (
                                    format!("/var/containers/c-{}/{}", container_id, dir_name),
                                    format!("/{}", dir_name),
                                    true,
                                    0u64,
                                ));
                            }
                        }
                    }
                }

                self.pid_to_container_id.insert(pid, container_id);
                self.container_owner_pids.insert(pid);
                // parent_cid is caller_container_id (or 0 if detach=true) — set
                // at the top of handle_container_run.  Assert child view ⊆ parent.
                self.install_view_and_run(thread_token, &view_mounts, requested_profile, container_id, parent_cid, false);
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
                    image_name.clone()
                };
                let effective_parent = if image_name == "vt" && parent_cid == 0 && self.vtmgr_container_id != 0 {
                    self.vtmgr_container_id
                } else {
                    parent_cid
                };
                let run_session_id = self.resolve_caller_session(sender_tid)
                    .map(|s| s.container_id).unwrap_or(0);
                let inst_name = self.next_instance_name(run_session_id, &display_name);
                self.container_instances.insert(container_id, ContainerInstance {
                    name: display_name,
                    instance_name: inst_name,
                    session_id: run_session_id,
                    container_id,
                    parent_container_id: effective_parent,
                    pid,
                    image_path: image_dir,
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                    restart_policy,
                    restart_count: 0,
                    last_exit_code: 0,
                    restart_attempt_start: 0,
                    quota: QuotaSpec::default(),
                    live_processes: 0,
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

    fn handle_container_list(&mut self, msg: &Message, sender_tid: usize) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(PROCMGR_CONTAINER_LIST_LABEL, [0; 6], 1);

        // Determine caller visibility
        let caller_pid = self.tid_to_pid.get(&sender_tid).copied().unwrap_or(0);
        let caller_profile = self.pid_to_profile.get(&caller_pid).copied()
            .unwrap_or(CapProfile::empty());
        let session_profile = self.resolve_caller_session(sender_tid)
            .map(|s| s.profile)
            .unwrap_or(CapProfile::empty());
        let is_admin = caller_profile.contains(CapProfile::ADMIN)
            || session_profile.contains(CapProfile::ADMIN);
        let caller_session_cid = self.resolve_caller_session(sender_tid)
            .map(|s| s.container_id)
            .unwrap_or(0);

        let mut listing = String::new();
        for inst in self.container_instances.values() {
            // Session filtering: admin sees all, user sees own session + system
            if !is_admin && inst.session_id != 0 && inst.session_id != caller_session_cid {
                continue;
            }
            listing.push_str(&format!("{} {} {} {}\n",
                inst.instance_name, inst.pid, inst.container_id, inst.session_id));
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

/// Pack a resolved envelope env map into the wire format procmgr expects:
/// "KEY=VALUE\0KEY=VALUE\0...". Returns (packed_bytes, count).
fn build_envelope_env_payload(
    env: &alloc::collections::BTreeMap<alloc::string::String, alloc::string::String>,
) -> (Vec<u8>, usize) {
    let mut payload = Vec::new();
    let mut count = 0;
    for (k, v) in env {
        payload.extend_from_slice(k.as_bytes());
        payload.push(b'=');
        payload.extend_from_slice(v.as_bytes());
        payload.push(0);
        count += 1;
    }
    (payload, count)
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

/// Extract the cwd string from the end of a spawn payload.
///
/// Returns `(payload_without_trailer, cwd_bytes)`. If no trailer is present,
/// returns the full payload and an empty byte slice.
fn split_cwd_trailer(payload: &[u8]) -> (&[u8], &[u8]) {
    if payload.len() < 8 {
        return (payload, &[]);
    }
    let magic_pos = payload.len() - 4;
    let magic_bytes: [u8; 4] = match payload[magic_pos..].try_into() {
        Ok(b) => b,
        Err(_) => return (payload, &[]),
    };
    if u32::from_le_bytes(magic_bytes) != SPAWN_CWD_MAGIC {
        return (payload, &[]);
    }

    let len_pos = magic_pos - 4;
    let len_bytes: [u8; 4] = match payload[len_pos..magic_pos].try_into() {
        Ok(b) => b,
        Err(_) => return (payload, &[]),
    };
    let cwd_len = u32::from_le_bytes(len_bytes) as usize;

    if cwd_len > len_pos {
        return (payload, &[]);
    }
    if cwd_len > CWD_MAX {
        // CWD_MAX guardrail — drop obviously malformed trailers.
        return (payload, &[]);
    }

    let cwd_start = len_pos - cwd_len;
    (&payload[..cwd_start], &payload[cwd_start..len_pos])
}

/// Strip the optional ARGV trailer `[u32 argv_bytes_len LE][u32 ARGV_MAGIC LE]`
/// from the end of `effective_payload` (which must already have the CWD trailer
/// stripped). Returns `(remaining, argv_bytes)`. If no ARGV magic is present,
/// returns `(payload, &[])` — the empty-argv case.
///
/// Bound argv_bytes_len against `payload.len() - 8` to reject malformed trailers.
fn split_argv_trailer(payload: &[u8]) -> (&[u8], &[u8]) {
    if payload.len() < 8 {
        return (payload, &[]);
    }
    let magic_pos = payload.len() - 4;
    let magic_bytes: [u8; 4] = match payload[magic_pos..].try_into() {
        Ok(b) => b,
        Err(_) => return (payload, &[]),
    };
    if u32::from_le_bytes(magic_bytes) != libcluu::ipc::ARGV_MAGIC {
        return (payload, &[]);
    }

    let len_pos = magic_pos - 4;
    let len_bytes: [u8; 4] = match payload[len_pos..magic_pos].try_into() {
        Ok(b) => b,
        Err(_) => return (payload, &[]),
    };
    let argv_bytes_len = u32::from_le_bytes(len_bytes) as usize;

    if argv_bytes_len > len_pos {
        return (payload, &[]);
    }
    // Reject trailers larger than the ProcessInfo page can plausibly carry —
    // the child's argv must fit inside the 4 KB ProcessInfo page alongside
    // ProcessInfo headers, the cwd block, and a small name margin. 3 KB is a
    // conservative ceiling; libcluu::args also bails on argv_offset >= PAGE_SIZE.
    if argv_bytes_len > MAX_ARGV_TRAILER_BYTES {
        return (payload, &[]);
    }

    // A well-formed trailer always carries at least one NUL-terminated arg.
    // Reject degenerate (zero-length or missing terminal NUL) trailers rather
    // than under-counting argc downstream.
    if argv_bytes_len == 0 || payload[len_pos - 1] != 0 {
        return (payload, &[]);
    }

    let argv_start = len_pos - argv_bytes_len;
    (&payload[..argv_start], &payload[argv_start..len_pos])
}

/// Strip the optional ENV trailer from the end of `payload` (which must
/// already have the CWD trailer stripped, since ENV sits between REDIR and
/// CWD on the wire). Returns `(remaining, env_bytes)`. If no ENV magic is
/// present, returns `(payload, &[])` and procmgr falls back to `DEFAULT_ENV`.
///
/// Wire format:
///   `[env_bytes][u32 env_bytes_len LE][u32 ENV_MAGIC LE]`
/// where `env_bytes` is "KEY=VALUE\0KEY=VALUE\0..." packed.
fn split_env_trailer(payload: &[u8]) -> (&[u8], &[u8]) {
    if payload.len() < 8 {
        return (payload, &[]);
    }
    let magic_pos = payload.len() - 4;
    let magic_bytes: [u8; 4] = match payload[magic_pos..].try_into() {
        Ok(b) => b,
        Err(_) => return (payload, &[]),
    };
    if u32::from_le_bytes(magic_bytes) != SPAWN_ENV_MAGIC {
        return (payload, &[]);
    }

    let len_pos = magic_pos - 4;
    let len_bytes: [u8; 4] = match payload[len_pos..magic_pos].try_into() {
        Ok(b) => b,
        Err(_) => return (payload, &[]),
    };
    let env_bytes_len = u32::from_le_bytes(len_bytes) as usize;

    if env_bytes_len > len_pos {
        return (payload, &[]);
    }
    // Sanity cap: env data must fit in the child's ProcessInfo page alongside
    // headers, argv, and cwd. 3 KB is a generous ceiling — same envelope cap
    // we use for argv.
    if env_bytes_len > MAX_ARGV_TRAILER_BYTES {
        return (payload, &[]);
    }
    // Well-formed env trailers always end with a NUL terminator.
    if env_bytes_len == 0 || payload[len_pos - 1] != 0 {
        return (payload, &[]);
    }

    let env_start = len_pos - env_bytes_len;
    (&payload[..env_start], &payload[env_start..len_pos])
}

/// Strip the optional REDIR trailer from the end of `payload` (which must already
/// have the CWD trailer stripped). Returns `(remaining, redir_entries_bytes)`.
/// If no REDIR magic is present, returns `(payload, &[])`.
///
/// Wire format (at end of payload after ARGV, before CWD was already stripped):
///   `[redir entries...]`
///   `[u32 entries_len LE][u32 REDIR_MAGIC LE]`
fn split_redir_trailer(payload: &[u8]) -> (&[u8], &[u8]) {
    if payload.len() < 8 {
        return (payload, &[]);
    }
    let magic_pos = payload.len() - 4;
    let magic_bytes: [u8; 4] = match payload[magic_pos..].try_into() {
        Ok(b) => b,
        Err(_) => return (payload, &[]),
    };
    if u32::from_le_bytes(magic_bytes) != SPAWN_REDIR_MAGIC {
        return (payload, &[]);
    }

    let len_pos = magic_pos - 4;
    let len_bytes: [u8; 4] = match payload[len_pos..magic_pos].try_into() {
        Ok(b) => b,
        Err(_) => return (payload, &[]),
    };
    let entries_len = u32::from_le_bytes(len_bytes) as usize;

    if entries_len > len_pos {
        return (payload, &[]);
    }
    // Sanity cap: max 4 entries, each at most 4 + 255 = 259 bytes → ~1 KB.
    const MAX_REDIR_TRAILER_BYTES: usize = 4 * 259;
    if entries_len > MAX_REDIR_TRAILER_BYTES {
        return (payload, &[]);
    }

    let entries_start = len_pos - entries_len;
    (&payload[..entries_start], &payload[entries_start..len_pos])
}

/// Ask VFS to clone a parent's open file to a child client_id and mint a
/// narrowed VFS token.
///
/// `vfs_endpoint`   — procmgr's IPC_SEND|IPC_CALL token to vfs:main
/// `parent_cid`     — parent's client_id in VFS (= parent's thread TID)
/// `parent_rfd`     — VFS-side fd number for the parent's open file
/// `child_rights`   — rights bits to narrow to (e.g. READ|WRITE)
/// `child_tid`      — new thread id; used as new client_id for the child in VFS
///
/// Returns `(derived_handle, child_client_id, child_remote_fd)` on success.
///
/// VFS reply wire:
///   words[0] = status (0 or errno)
///   words[1] = derived token handle
///   words[2] = child_client_id (echo of child_tid)
///   words[3] = child_remote_fd (freshly allocated fd slot under child_client_id)
fn vfs_derive_child_fd(
    vfs_endpoint: usize,
    parent_cid: usize,
    parent_rfd: usize,
    child_rights: usize,
    child_tid: usize,
) -> Result<(usize, usize, usize)> {
    if vfs_endpoint == 0 {
        return Err(Error::NotFound);
    }
    let mut msg = Message::new(
        ipc::VFS_DERIVE_CHILD_FD_LABEL,
        [parent_cid, parent_rfd, child_rights, child_tid, 0, 0],
        4,
    );
    ipc::call(vfs_endpoint, &mut msg, IpcFlags::empty())?;
    if msg.words[0] != 0 {
        return Err(Error::from_errno(msg.words[0] as isize));
    }
    Ok((msg.words[1], msg.words[2], msg.words[3]))
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
    cwd_bytes: &[u8],
    redir_bytes: &[u8],
    // Per-fd VFS metadata for fds 0..3.  Each entry is `(client_id, remote_fd)`.
    // Both fields are 0 for legacy (tty/pipe) fds.  Written into the ProcessInfo
    // page trailer at PARAM_FD_VFS_OFFSET so that the child's init_stdio can
    // build FdEntry::file(...) for VFS-backed fds.
    fd_vfs_meta: &[(usize, usize); 4],
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

    let mut params = [0u64; 16];
    // params[0] = pipe_mask for all processes
    params[0] = pipe_mask as u64;
    // NOTE: PARAM_CAP_PROFILE (slot 5) is NOT written here. The cap profile is
    // tracked server-side in pid_to_profile. Slot 5 is shared with
    // PARAM_CONSOLE_INSTANCE for console, so writing the profile here would
    // corrupt the instance ID. Callers use param_overrides for slot 5 if needed.

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

    // Place cwd bytes in the page AFTER whatever was actually written. If env
    // didn't fit it wasn't copied into the page, so anchor cwd just after argv
    // so cwd still gets a chance to land. Clamp to CWD_MAX and guard against
    // overflow of the 4 KB page. If it won't fit, silently emit zero length —
    // child falls back to "/".
    let cwd_data_offset = if env_fits {
        env_data_offset + env_data.len()
    } else {
        argv_data_offset + argv_payload.len()
    };
    let cwd_clamped_len = cwd_bytes.len().min(CWD_MAX);
    let cwd_end = cwd_data_offset + cwd_clamped_len;
    let cwd_fits = cwd_clamped_len > 0 && cwd_end <= PAGE_SIZE;

    if cwd_fits {
        params[PARAM_CWD_OFFSET] = cwd_data_offset as u64;
        params[PARAM_CWD_LEN] = cwd_clamped_len as u64;
    }

    // Place redir bytes in the page AFTER cwd. Clamp to a reasonable ceiling
    // (max 4 entries * (4 + 255) bytes ≈ 1 KB) and guard against page overflow.
    let redir_data_offset = if cwd_fits {
        cwd_data_offset + cwd_clamped_len
    } else {
        cwd_data_offset
    };
    let redir_end = redir_data_offset + redir_bytes.len();
    let redir_fits = !redir_bytes.is_empty() && redir_end <= PAGE_SIZE;

    if redir_fits {
        params[PARAM_REDIR_OFFSET] = redir_data_offset as u64;
        params[PARAM_REDIR_LEN] = redir_bytes.len() as u64;
    }

    // Place VFS fd trailer AFTER redir bytes (if any VFS-backed fds present).
    // Layout: 4 × 16 bytes = 64 bytes total.
    //   entry i (fd i): [u64 vfs_client_id LE][u64 vfs_remote_fd LE]
    // Entry with vfs_client_id == 0 means fd i is NOT VFS-backed.
    const FD_VFS_TRAILER_SIZE: usize = 4 * 16; // 64 bytes
    let vfs_trailer_data_offset = if redir_fits {
        redir_data_offset + redir_bytes.len()
    } else {
        redir_data_offset
    };
    let vfs_trailer_end = vfs_trailer_data_offset + FD_VFS_TRAILER_SIZE;
    // Only write VFS trailer if any fd has non-zero client_id (i.e. is VFS-backed).
    let any_vfs = fd_vfs_meta.iter().any(|&(cid, _)| cid != 0);
    let vfs_trailer_fits = any_vfs && vfs_trailer_end <= PAGE_SIZE;

    if vfs_trailer_fits {
        params[PARAM_FD_VFS_OFFSET] = vfs_trailer_data_offset as u64;
        params[PARAM_FD_VFS_LEN] = FD_VFS_TRAILER_SIZE as u64;
    }

    // Apply caller-specified param overrides LAST (e.g. console instance/active).
    for &(idx, val) in param_overrides {
        // Belt-and-suspenders: slots 10/11 (PARAM_CWD_OFFSET/LEN),
        // slots 12/13 (PARAM_REDIR_OFFSET/LEN), and slots 14/15 (PARAM_FD_VFS_OFFSET/LEN)
        // are procmgr-trusted.  Callers that reach this loop are already vetted, but keep
        // the guard so a future caller can't accidentally forge these metadata slots.
        if idx == PARAM_CWD_OFFSET || idx == PARAM_CWD_LEN
            || idx == PARAM_REDIR_OFFSET || idx == PARAM_REDIR_LEN
            || idx == PARAM_FD_VFS_OFFSET || idx == PARAM_FD_VFS_LEN
        {
            continue;
        }
        if idx < params.len() {
            params[idx] = val;
        }
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

    // Write cwd bytes after env data
    if cwd_fits {
        page[cwd_data_offset..cwd_end].copy_from_slice(&cwd_bytes[..cwd_clamped_len]);
    }

    // Write redir bytes after cwd
    if redir_fits {
        page[redir_data_offset..redir_end].copy_from_slice(redir_bytes);
    }

    // Write VFS fd trailer after redir bytes.
    // Each entry: [u64 vfs_client_id LE][u64 vfs_remote_fd LE]
    if vfs_trailer_fits {
        let dst = &mut page[vfs_trailer_data_offset..vfs_trailer_end];
        for (i, &(cid, rfd)) in fd_vfs_meta.iter().enumerate() {
            let off = i * 16;
            dst[off..off + 8].copy_from_slice(&(cid as u64).to_le_bytes());
            dst[off + 8..off + 16].copy_from_slice(&(rfd as u64).to_le_bytes());
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

    // DEVICE: needs THREAD_CONTROL for interrupt handling threads, and
    // SPACE_MAP on the space token so the driver can map MMIO regions
    // (framebuffer, PCI BARs, etc.) into its address space.
    if profile.contains(CapProfile::DEVICE) {
        r[TOKEN_SELF] |= Rights::THREAD_CONTROL;
        r[TOKEN_SPACE] |= Rights::SPACE_MAP;
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

/// Parse a RestartPolicy from a manifest TOML document's [lifecycle] section.
fn parse_restart_policy(doc: &libcluu::toml::TomlDoc) -> RestartPolicy {
    if let Some(lifecycle) = doc.table("lifecycle") {
        match lifecycle.get_str("restart_policy") {
            Some("always") => RestartPolicy::Always,
            Some("on_failure") => {
                let max = lifecycle.get_str("max_restarts")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(DEFAULT_MAX_RESTARTS_ON_FAILURE);
                let window = lifecycle.get_str("restart_window_secs")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(DEFAULT_RESTART_WINDOW_SECS);
                RestartPolicy::OnFailure { max_restarts: max, window_secs: window }
            }
            _ => RestartPolicy::Never,
        }
    } else {
        RestartPolicy::Never
    }
}

/// Resolve a virtual path through a VFS view's mount table.
/// Returns the concrete filesystem path, or None if not visible in the view.
fn resolve_path_in_view(path: &str, view: &[(String, String, bool, u64)]) -> Option<String> {
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

    for (src, dst, _writable, _memfs_cid) in view {
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
/// Returns a list of (src, dst, writable, memfs_cid) tuples.  For Phase C these
/// are identity mappings (src == dst); Phase D will add path remapping.  All
/// `memfs_cid` values are 0 (resolve via global MountTable); the per-container
/// MemFs prepend is currently still added by VFS at view-set time (see Task 7).
fn default_view_for_profile(profile: CapProfile) -> ViewMountList {
    libcluu::vfs_view::default_mounts_for_profile(profile)
        .iter()
        .map(|&(src, dst, w)| (String::from(src), String::from(dst), w, 0u64))
        .collect()
}

/// Build the per-container specific system mounts that were previously created
/// inside VFS's set_view. These are prepended to the view so they take
/// priority under VFS's first-match-wins resolution:
///   /data  — MountTable-backed, path = /var/containers/c-<cid>/data, writable
/// The `/` catch-all is NOT returned here because it must be *appended* to
/// the view (see `container_catchall_mount`) — if prepended, it would shadow
/// every other mount under first-match-wins.
fn container_system_mounts(container_id: u64) -> ViewMountList {
    if container_id == 0 {
        return Vec::new();
    }
    alloc::vec![(
        format!("/var/containers/c-{}/data", container_id),
        String::from("/data"),
        true,
        0, // MountTable — persistent/ext2-backed via MountTable
    )]
}

/// Build the catch-all `/ → MemFs { own_cid }` mount. Appended at the END of
/// the view so that every more specific mount takes precedence. Returns an
/// empty list for top-level (container_id == 0).
fn container_catchall_mount(container_id: u64) -> ViewMountList {
    if container_id == 0 {
        return Vec::new();
    }
    alloc::vec![(
        String::from("/"),
        String::from("/"),
        true,
        container_id,
    )]
}

/// Build /tmp and /log mounts for this container given the resolved policy.
/// - Private or no parent → memfs_cid = own container_id (fresh MemFs)
/// - Inherit with parent  → memfs_cid = caller_container_id (parent's MemFs)
/// - Ro                   → same as Inherit but writable=false (stretch goal)
fn policy_driven_memfs_mounts(
    policies: &[MountPolicyEntry],
    own_cid: u64,
    parent_cid: u64,
) -> ViewMountList {
    let mut out = ViewMountList::new();
    for entry in policies {
        // Only /tmp and /log are MemFs-backed today. If the user declares a
        // MOUNT on some other path, it has no effect here (we fall through
        // to view passthrough, which already inherits by default).
        if entry.path != "/tmp" && entry.path != "/log" {
            continue;
        }
        let (cid, writable) = match entry.policy {
            MountPolicy::Inherit if parent_cid != 0 => (parent_cid, true),
            MountPolicy::Inherit => (own_cid, true), // top-level: no parent to inherit from
            MountPolicy::Private => (own_cid, true),
            MountPolicy::Ro if parent_cid != 0 => (parent_cid, false),
            MountPolicy::Ro => (own_cid, false),
        };
        out.push((entry.path.clone(), entry.path.clone(), writable, cid));
    }
    out
}

/// Check whether `child_view` is a valid narrowing of `parent_view`.
///
/// Every mount in the child view must be covered by a mount in the parent view
/// with an equal or wider prefix and equal or greater write permission.
/// An empty parent view means no filesystem access — no child mount can pass.
/// An empty child view is always valid (child requests no access).
#[allow(dead_code)]
fn can_narrow_view(parent_view: &[(String, String, bool, u64)], child_view: &[(String, String, bool, u64)]) -> bool {
    first_view_violation(parent_view, child_view).is_none()
}

/// Return the first child mount that is not covered by `parent_view`, or `None`
/// if the child view is a valid narrowing.  Returns `(src, dst, writable)`.
fn first_view_violation<'a>(
    parent_view: &[(String, String, bool, u64)],
    child_view: &'a [(String, String, bool, u64)],
) -> Option<(&'a str, &'a str, bool)> {
    for (child_src, child_dst, child_writable, _child_memfs_cid) in child_view {
        let covered = parent_view.iter().any(|(p_src, p_dst, p_writable, _p_memfs_cid)| {
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
            return Some((child_src.as_str(), child_dst.as_str(), *child_writable));
        }
    }
    None
}

/// Serialize and send a VFS_SET_VIEW message for a newly spawned child.
///
/// `client_tid` is the child's kernel `ThreadId` (the `sender_tid` seen by VFS).
/// `mounts` is the list of (src, dst, writable, memfs_cid) tuples from
/// `default_view_for_profile` (or a derived view).  A `client_tid` of 0 means
/// "sender_tid" (manager self-view bootstrap path).
fn send_vfs_set_view(
    vfs_endpoint: usize,
    client_tid: usize,
    mounts: &[(String, String, bool, u64)],
    profile: CapProfile,
    container_id: u64,
) -> Result<()> {
    if vfs_endpoint == 0 {
        return Ok(());
    }

    // Wire format (per mount):
    //   u16 src_len LE | u16 dst_len LE | u8 flags | u64 memfs_cid LE |
    //   src_bytes       | dst_bytes
    //
    // flags: bit 0 = writable. `memfs_cid = 0` means MountTable; non-zero
    // means MountTarget::MemFs { container_id: memfs_cid }.
    let mut payload = Vec::new();
    for (src, dst, writable, memfs_cid) in mounts {
        let src_bytes = src.as_bytes();
        let dst_bytes = dst.as_bytes();
        payload.extend_from_slice(&(src_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(&(dst_bytes.len() as u16).to_le_bytes());
        payload.push(if *writable { 1u8 } else { 0u8 });
        payload.extend_from_slice(&memfs_cid.to_le_bytes());
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

