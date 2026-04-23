//! Process-related syscall stubs.

use super::{c_char, c_int, pid_t};
use crate::errno::{set_errno, ECHILD, EINVAL, ENOSYS, ESRCH};
use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

const PROCMGR_SPAWN_LABEL: u32 = 2;
const PROCMGR_KILL_LABEL: u32 = 3;
const PROCMGR_EXIT_LABEL: u32 = 1;
const WNOHANG: c_int = 1;

struct WaitState {
    notify_endpoint: usize,
    procmgr_endpoint: usize,
    cookie_to_pid: BTreeMap<usize, pid_t>,
    pid_to_cookie: BTreeMap<pid_t, usize>,
    pending_exit_events: VecDeque<(pid_t, i32)>,
}

impl WaitState {
    fn new() -> Self {
        Self {
            notify_endpoint: 0,
            procmgr_endpoint: 0,
            cookie_to_pid: BTreeMap::new(),
            pid_to_cookie: BTreeMap::new(),
            pending_exit_events: VecDeque::new(),
        }
    }

    fn push_exit_event(&mut self, pid: pid_t, status: i32) {
        self.pending_exit_events.push_back((pid, status));
    }

    fn pop_exit_event_for_pid(&mut self, target_pid: pid_t) -> Option<(pid_t, i32)> {
        if target_pid == -1 {
            return self.pending_exit_events.pop_front();
        }
        let index = self
            .pending_exit_events
            .iter()
            .position(|(pid, _)| *pid == target_pid)?;
        self.pending_exit_events.remove(index)
    }

    fn has_known_child(&self, pid: pid_t) -> bool {
        if pid == -1 {
            return !self.pid_to_cookie.is_empty() || !self.pending_exit_events.is_empty();
        }
        self.pid_to_cookie.contains_key(&pid)
            || self
                .pending_exit_events
                .iter()
                .any(|(event_pid, _)| *event_pid == pid)
    }
}

lazy_static! {
    static ref WAIT_STATE: Mutex<WaitState> = Mutex::new(WaitState::new());
}

fn clear_cached_procmgr_endpoint() {
    let mut state = WAIT_STATE.lock();
    state.procmgr_endpoint = 0;
}

fn get_cached_or_lookup_procmgr_endpoint() -> Option<usize> {
    {
        let state = WAIT_STATE.lock();
        if state.procmgr_endpoint != 0 {
            return Some(state.procmgr_endpoint);
        }
    }

    let endpoint = crate::registry::lookup_service("procmgr:spawn")?;
    let mut state = WAIT_STATE.lock();
    state.procmgr_endpoint = endpoint;
    Some(endpoint)
}

/// Exit the current process.
///
/// # Arguments
/// - `status`: Exit status code
#[no_mangle]
pub extern "C" fn _exit(status: c_int) -> ! {
    // Notify parent via IPC
    let _ = crate::ipc::notify_exit(status);

    // Block forever waiting for procmgr to clean us up
    let info = crate::boot::process_info();
    let ipc_cap = info.tokens[crate::boot::TOKEN_IPC];
    if ipc_cap != 0 {
        if let Ok(ep) = crate::syscall::endpoint_create(ipc_cap) {
            let mut buf = [0u8; 64];
            let _ = crate::syscall::ipc_recv(ep, &mut buf);
        }
    }

    // Fallback: yield loop
    loop {
        let _ = crate::syscall::yield_cpu();
    }
}

/// Get process ID.
///
/// # Returns
/// Current process ID.
#[no_mangle]
pub extern "C" fn _getpid() -> pid_t {
    crate::boot::pid() as pid_t
}

/// Send signal to a process.
///
/// In CLUU, this sends a PROC_KILL IPC message to procmgr.
/// Supported signals currently include:
/// - `SIGKILL`/`SIGTERM`/`SIGINT`: terminate target process thread
/// - `SIGSTOP`/`SIGCONT`: suspend/resume target process thread
///
/// # Arguments
/// - `pid`: Target process ID
/// - `sig`: Signal number
///
/// # Returns
/// 0 on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn _kill(pid: pid_t, sig: c_int) -> c_int {
    if pid <= 0 {
        set_errno(ESRCH);
        return -1;
    }

    // Get procmgr endpoint from cache/registry
    let procmgr_ep = match get_cached_or_lookup_procmgr_endpoint() {
        Some(ep) => ep,
        None => {
            set_errno(ESRCH);
            return -1;
        }
    };

    // Send PROC_KILL message
    // Label = 3 (PROC_KILL), words[0] = pid, words[1] = signal
    let do_call = |endpoint: usize| {
        let mut msg = crate::types::Message::new(
            PROCMGR_KILL_LABEL,
            [pid as usize, sig as usize, 0, 0, 0, 0],
            2,
        );
        match crate::ipc::call(endpoint, &mut msg, crate::IpcFlags::empty()) {
            Ok(()) => {
                let status = msg.words[0] as isize;
                if status < 0 {
                    set_errno((-status) as i32);
                    -1
                } else {
                    0
                }
            }
            Err(err) => crate::errno::return_error_i32(err),
        }
    };

    let rc = do_call(procmgr_ep);
    if rc == -1 {
        // Refresh endpoint once in case procmgr endpoint token was rotated.
        clear_cached_procmgr_endpoint();
        if let Some(ep) = get_cached_or_lookup_procmgr_endpoint() {
            return do_call(ep);
        }
    }

    rc
}

/// Fork the current process.
///
/// Not supported in CLUU - use `posix_spawn()` instead.
///
/// # Returns
/// Always returns -1 with errno = ENOSYS.
#[no_mangle]
pub extern "C" fn _fork() -> pid_t {
    set_errno(ENOSYS);
    -1
}

/// Execute a program.
///
/// Not supported in CLUU - use `posix_spawn()` instead.
///
/// # Returns
/// Always returns -1 with errno = ENOSYS.
#[no_mangle]
pub extern "C" fn _execve(
    _path: *const c_char,
    _argv: *const *const c_char,
    _envp: *const *const c_char,
) -> c_int {
    set_errno(ENOSYS);
    -1
}

/// Wait for a child process to exit.
///
/// Blocks until any child exits and returns its status.
///
/// # Arguments
/// - `status`: Pointer to store exit status (can be NULL)
///
/// # Returns
/// Child PID on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn _wait(_status: *mut c_int) -> pid_t {
    waitpid(-1, _status, 0)
}

/// Wait for a specific child process.
///
/// # Arguments
/// - `pid`: Process ID to wait for (-1 = any child)
/// - `status`: Pointer to store exit status
/// - `options`: Wait options (WNOHANG, etc.)
///
/// # Returns
/// Child PID on success, 0 if WNOHANG and no child exited, -1 on error.
#[no_mangle]
pub extern "C" fn waitpid(pid: pid_t, status: *mut c_int, options: c_int) -> pid_t {
    let supported = WNOHANG;
    if options & !supported != 0 {
        set_errno(EINVAL);
        return -1;
    }
    if pid < -1 || pid == 0 {
        set_errno(EINVAL);
        return -1;
    }

    let mut state = WAIT_STATE.lock();
    if state.notify_endpoint == 0 {
        set_errno(ECHILD);
        return -1;
    }
    let notify_endpoint = state.notify_endpoint;

    if let Some((done_pid, exit_code)) = state.pop_exit_event_for_pid(pid) {
        if !status.is_null() {
            unsafe {
                *status = exit_code;
            }
        }
        drop(state);
        super::signal::raise(super::signal::SIGCHLD);
        return done_pid;
    }

    if !state.has_known_child(pid) {
        set_errno(ECHILD);
        return -1;
    }

    if options & WNOHANG != 0 {
        return 0;
    }

    drop(state);

    loop {
        let mut buf = [0u8; core::mem::size_of::<crate::types::Message>()];
        let len = match crate::syscall::ipc_recv(notify_endpoint, &mut buf) {
            Ok(len) => len,
            Err(err) => {
                set_errno(crate::errno::from_cluu_error(err));
                return -1;
            }
        };

        if len < core::mem::size_of::<crate::types::Message>() {
            continue;
        }

        let msg = unsafe { (buf.as_ptr() as *const crate::types::Message).read_unaligned() };
        if msg.tag.label != PROCMGR_EXIT_LABEL || msg.tag.words < 2 {
            continue;
        }

        let cookie = msg.words[0];
        let exit_code = msg.words[1] as i32;

        let mut state = WAIT_STATE.lock();
        let child_pid = match state.cookie_to_pid.remove(&cookie) {
            Some(done_pid) => done_pid,
            None => continue,
        };
        state.pid_to_cookie.remove(&child_pid);
        state.push_exit_event(child_pid, exit_code);

        if let Some((done_pid, done_status)) = state.pop_exit_event_for_pid(pid) {
            if !status.is_null() {
                unsafe {
                    *status = done_status;
                }
            }
            drop(state);
            super::signal::raise(super::signal::SIGCHLD);
            return done_pid;
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// posix_spawn - The preferred process creation API for CLUU
// ═══════════════════════════════════════════════════════════════════════════

/// Heap-allocated backing storage for posix_spawn_file_actions_t.
///
/// Newlib defines `posix_spawn_file_actions_t` as a *pointer* type
/// (`struct __posix_spawn_file_actions *`), so the C ABI passes
/// `posix_spawn_file_actions_t *` == `**FileActionsInner` across FFI.
/// We heap-allocate this in _init and free it in _destroy.
#[repr(C)]
pub struct FileActionsInner {
    count: u8,
    _pad: [u8; 7],
    actions: [FdAction; MAX_FD_ACTIONS],
}

/// Spawn a new process.
///
/// This is the primary way to create processes in CLUU. It maps to procmgr's
/// spawn IPC protocol.
///
/// # Arguments
/// - `pid`: Pointer to store child PID
/// - `path`: Path to executable
/// - `file_actions`: File descriptor actions (can be NULL)
/// - `attrp`: Spawn attributes (can be NULL)
/// - `argv`: Argument vector (NULL-terminated)
/// - `envp`: Environment vector (NULL-terminated, can be NULL)
///
/// # Returns
/// 0 on success, positive errno on error.
#[no_mangle]
pub extern "C" fn posix_spawn(
    pid: *mut pid_t,
    path: *const c_char,
    _file_actions: *const *const FileActionsInner,
    _attrp: *const core::ffi::c_void,
    _argv: *const *const c_char,
    _envp: *const *const c_char,
) -> c_int {
    if path.is_null() {
        return EINVAL;
    }

    // Convert path to Rust str
    let _path_str = unsafe {
        let mut len = 0;
        let mut p = path;
        while *p != 0 {
            len += 1;
            p = p.add(1);
        }
        match core::str::from_utf8(core::slice::from_raw_parts(path as *const u8, len)) {
            Ok(s) => s,
            Err(_) => return EINVAL,
        }
    };

    // Get procmgr endpoint
    let procmgr_ep = match get_cached_or_lookup_procmgr_endpoint() {
        Some(ep) => ep,
        None => return ENOSYS,
    };

    let notify_endpoint = {
        let mut state = WAIT_STATE.lock();
        if state.notify_endpoint == 0 {
            let ipc_cap = crate::boot::process_info().tokens[crate::boot::TOKEN_IPC];
            if ipc_cap == 0 {
                return ENOSYS;
            }
            match crate::syscall::endpoint_create(ipc_cap) {
                Ok(ep) => state.notify_endpoint = ep,
                Err(err) => return crate::errno::from_cluu_error(err),
            }
        }
        state.notify_endpoint
    };

    // Send spawn request to procmgr
    let mut payload = Vec::new();
    push_cstr(path, &mut payload);

    let mut argc = 0usize;
    if !_argv.is_null() {
        unsafe {
            let mut p = _argv;
            while !(*p).is_null() {
                push_cstr(*p, &mut payload);
                argc += 1;
                p = p.add(1);
            }
        }
    }

    // Serialize environment variables into payload after argv data
    let env_payload_offset = payload.len();
    let mut envc = 0usize;

    // Use envp parameter if provided, otherwise use current environ
    let envp_to_use = if !_envp.is_null() {
        _envp
    } else {
        unsafe { super::env::environ as *const *const c_char }
    };

    if !envp_to_use.is_null() {
        unsafe {
            let mut p = envp_to_use;
            while !(*p).is_null() {
                push_cstr(*p, &mut payload);
                envc += 1;
                p = p.add(1);
            }
        }
    }

    // Serialize fd actions (FDAC) after env data
    let fdac_offset = serialize_fd_actions(_file_actions, &mut payload);

    // Append the CWD magic trailer so procmgr can seed the child's cwd.
    // Layout: [cwd_bytes][u32 cwd_len LE][u32 CWD_MAGIC LE].
    let cwd_string = crate::posix::dir::current_dir_string();
    let cwd_bytes = cwd_string.as_bytes();
    let cwd_len = cwd_bytes.len().min(crate::boot::CWD_MAX);
    payload.extend_from_slice(&cwd_bytes[..cwd_len]);
    payload.extend_from_slice(&(cwd_len as u32).to_le_bytes());
    payload.extend_from_slice(&CWD_MAGIC.to_le_bytes());

    let mut msg = crate::types::Message::new(PROCMGR_SPAWN_LABEL, [0; 6], 6);
    msg.words[0] = payload.len();
    msg.words[1] = argc;
    msg.words[2] = fdac_offset; // FDAC payload offset (0 = no fd actions)
    msg.words[3] = notify_endpoint;
    msg.words[4] = envc;
    msg.words[5] = if envc > 0 { env_payload_offset } else { 0 };

    let mut reply = crate::types::Message::new(0, [0; 6], 0);
    let mut try_call =
        |endpoint: usize| crate::ipc::call_with_payload(endpoint, &msg, &payload, &mut reply);

    let result = match try_call(procmgr_ep) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Refresh endpoint once in case procmgr endpoint token was rotated.
            clear_cached_procmgr_endpoint();
            if let Some(ep) = get_cached_or_lookup_procmgr_endpoint() {
                try_call(ep)
            } else {
                Err(crate::Error::NotFound)
            }
        }
    };

    match result {
        Ok(()) => {
            let status = reply.words[0] as isize;
            if status < 0 {
                return (-status) as c_int;
            }
            let child_pid = reply.words[1] as pid_t;
            let cookie = reply.words[2];
            if !pid.is_null() {
                unsafe {
                    *pid = child_pid;
                }
            }
            let mut state = WAIT_STATE.lock();
            state.cookie_to_pid.insert(cookie, child_pid);
            state.pid_to_cookie.insert(child_pid, cookie);
            0
        }
        Err(e) => crate::errno::from_cluu_error(e),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// File actions for fd inheritance across posix_spawn
// ═══════════════════════════════════════════════════════════════════════════

const MAX_FD_ACTIONS: usize = 4;
/// Magic marker for fd actions in spawn payload.
const FDAC_MAGIC: u32 = 0x46444143; // "FDAC"
/// Magic marker for the CWD trailer at the end of the spawn payload.
/// Bytes in little-endian order: 'C','W','D',' ' = 0x43, 0x57, 0x44, 0x20.
const CWD_MAGIC: u32 = 0x2044_5743;
const FDAC_FLAG_PIPE: u32 = 0x01;

/// A single fd redirection action.
#[repr(C)]
#[derive(Clone, Copy)]
struct FdAction {
    target_fd: u32,
    flags: u32,
    endpoint: usize,
}

/// Initialize file actions — heap-allocates the inner struct.
///
/// C passes `posix_spawn_file_actions_t *` which is `**FileActionsInner`.
#[no_mangle]
pub extern "C" fn posix_spawn_file_actions_init(actions: *mut *mut FileActionsInner) -> c_int {
    if actions.is_null() {
        return EINVAL;
    }
    let inner = alloc::boxed::Box::new(FileActionsInner {
        count: 0,
        _pad: [0; 7],
        actions: [FdAction {
            target_fd: 0,
            flags: 0,
            endpoint: 0,
        }; MAX_FD_ACTIONS],
    });
    unsafe {
        *actions = alloc::boxed::Box::into_raw(inner);
    }
    0
}

/// Destroy file actions — frees the heap-allocated inner struct.
#[no_mangle]
pub extern "C" fn posix_spawn_file_actions_destroy(actions: *mut *mut FileActionsInner) -> c_int {
    if actions.is_null() {
        return EINVAL;
    }
    unsafe {
        let inner = *actions;
        if !inner.is_null() {
            drop(alloc::boxed::Box::from_raw(inner));
            *actions = core::ptr::null_mut();
        }
    }
    0
}

/// Add a dup2 action: child's `newfd` will be redirected to the endpoint backing `fd`.
///
/// The source `fd` is looked up in the current process's fd table to extract
/// the IPC endpoint token and pipe flag.
#[no_mangle]
pub extern "C" fn posix_spawn_file_actions_adddup2(
    actions: *mut *mut FileActionsInner,
    fd: c_int,
    newfd: c_int,
) -> c_int {
    if actions.is_null() || !(0..=3).contains(&newfd) {
        return EINVAL;
    }

    let inner = unsafe {
        let ptr = *actions;
        if ptr.is_null() {
            return EINVAL;
        }
        &mut *ptr
    };
    let count = inner.count as usize;
    if count >= MAX_FD_ACTIONS {
        return EINVAL;
    }

    // Look up source fd to get endpoint and pipe flag
    let table = crate::fd_table::FD_TABLE.lock();
    let entry = match table.get(fd) {
        Some(e) => e,
        None => return crate::errno::EBADF,
    };

    let mut flags: u32 = 0;
    if entry.is_pipe() {
        flags |= FDAC_FLAG_PIPE;
    }
    let endpoint = entry.endpoint;
    drop(table);

    inner.actions[count] = FdAction {
        target_fd: newfd as u32,
        flags,
        endpoint,
    };
    inner.count = (count + 1) as u8;
    0
}

/// Serialize fd actions into the spawn payload. Returns the offset where FDAC starts.
fn serialize_fd_actions(
    file_actions: *const *const FileActionsInner,
    payload: &mut Vec<u8>,
) -> usize {
    if file_actions.is_null() {
        return 0;
    }
    let inner_ptr = unsafe { *file_actions };
    if inner_ptr.is_null() {
        return 0;
    }
    let inner = unsafe { &*inner_ptr };
    let count = inner.count as usize;
    if count == 0 {
        return 0;
    }

    let fdac_offset = payload.len();

    // Write FDAC magic + count
    payload.extend_from_slice(&FDAC_MAGIC.to_le_bytes());
    payload.extend_from_slice(&(count as u32).to_le_bytes());

    // Write FdAction array
    for i in 0..count {
        let action_bytes = unsafe {
            core::slice::from_raw_parts(
                &inner.actions[i] as *const FdAction as *const u8,
                core::mem::size_of::<FdAction>(),
            )
        };
        payload.extend_from_slice(action_bytes);
    }

    fdac_offset
}

fn push_cstr(ptr: *const c_char, out: &mut Vec<u8>) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let mut p = ptr;
        while *p != 0 {
            out.push(*p as u8);
            p = p.add(1);
        }
    }
    out.push(0);
}

/// posix_spawnp - spawn with PATH search.
///
/// For now, just calls posix_spawn directly (no PATH search).
#[no_mangle]
pub extern "C" fn posix_spawnp(
    pid: *mut pid_t,
    file: *const c_char,
    file_actions: *const *const FileActionsInner,
    attrp: *const core::ffi::c_void,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    // TODO: Search PATH for file
    posix_spawn(pid, file, file_actions, attrp, argv, envp)
}

/// Run a shell command.
///
/// Spawns `/bin/sh -c "command"` and waits for it to complete.
///
/// # Arguments
/// - `command`: Command string to execute (NULL = check if shell exists)
///
/// # Returns
/// Exit status of command, or -1 on error.
#[no_mangle]
pub extern "C" fn system(command: *const c_char) -> c_int {
    if command.is_null() {
        // Check if shell is available
        return 1; // Assume shell exists
    }

    // Spawn sh -c "command"
    let sh_path = c"/bin/sh".as_ptr() as *const c_char;
    let c_flag = c"-c".as_ptr() as *const c_char;
    let argv: [*const c_char; 4] = [sh_path, c_flag, command, core::ptr::null()];

    let mut child_pid: pid_t = 0;
    let ret = posix_spawn(
        &mut child_pid,
        sh_path,
        core::ptr::null(),
        core::ptr::null(),
        argv.as_ptr(),
        core::ptr::null(),
    );

    if ret != 0 {
        set_errno(ret);
        return -1;
    }

    // Wait for child
    let mut status: c_int = 0;
    if waitpid(child_pid, &mut status, 0) < 0 {
        return -1;
    }

    status
}
