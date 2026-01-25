//! Process-related syscall stubs.

use super::{c_char, c_int, pid_t};
use crate::errno::{set_errno, ECHILD, EINVAL, ENOSYS, ESRCH};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

const PROCMGR_SPAWN_LABEL: u32 = 2;
const PROCMGR_EXIT_LABEL: u32 = 1;

struct WaitState {
    notify_endpoint: usize,
    cookie_to_pid: BTreeMap<usize, pid_t>,
    pid_to_cookie: BTreeMap<pid_t, usize>,
    pending_exits: BTreeMap<pid_t, i32>,
}

impl WaitState {
    fn new() -> Self {
        Self {
            notify_endpoint: 0,
            cookie_to_pid: BTreeMap::new(),
            pid_to_cookie: BTreeMap::new(),
            pending_exits: BTreeMap::new(),
        }
    }
}

lazy_static! {
    static ref WAIT_STATE: Mutex<WaitState> = Mutex::new(WaitState::new());
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
    let proc_cap = info.tokens[crate::boot::TOKEN_PROC_CAP];
    if proc_cap != 0 {
        if let Ok(ep) = crate::syscall::endpoint_create(proc_cap) {
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
/// Only SIGKILL (9) and SIGTERM (15) actually terminate; others are no-ops.
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

    // Get procmgr endpoint from registry
    let procmgr_ep = match crate::registry::lookup_service("procmgr:spawn") {
        Some(ep) => ep,
        None => {
            set_errno(ESRCH);
            return -1;
        }
    };

    // Send PROC_KILL message
    // Label = 2 (PROC_KILL), words[0] = pid, words[1] = signal
    let mut msg = crate::types::Message::new(2, [pid as usize, sig as usize, 0, 0, 0, 0], 2);

    match crate::ipc::call(procmgr_ep, &mut msg, crate::IpcFlags::empty()) {
        Ok(()) => {
            // Check reply status
            let status = msg.words[0] as isize;
            if status < 0 {
                set_errno((-status) as i32);
                -1
            } else {
                0
            }
        }
        Err(e) => crate::errno::return_error_i32(e),
    }
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
pub extern "C" fn waitpid(pid: pid_t, status: *mut c_int, _options: c_int) -> pid_t {
    let mut state = WAIT_STATE.lock();
    if state.notify_endpoint == 0 {
        set_errno(ECHILD);
        return -1;
    }
    let notify_endpoint = state.notify_endpoint;

    if pid > 0 {
        if let Some(exit_code) = state.pending_exits.remove(&pid) {
            if !status.is_null() {
                unsafe {
                    *status = exit_code;
                }
            }
            return pid;
        }
        if !state.pid_to_cookie.contains_key(&pid) {
            set_errno(ECHILD);
            return -1;
        }
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
            Some(pid) => pid,
            None => continue,
        };
        state.pid_to_cookie.remove(&child_pid);

        if pid == -1 || pid == child_pid {
            if !status.is_null() {
                unsafe {
                    *status = exit_code;
                }
            }
            return child_pid;
        }

        state.pending_exits.insert(child_pid, exit_code);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// posix_spawn - The preferred process creation API for CLUU
// ═══════════════════════════════════════════════════════════════════════════

/// Opaque type for spawn file actions (fd redirections).
#[repr(C)]
pub struct posix_spawn_file_actions_t {
    _opaque: [u8; 64],
}

/// Opaque type for spawn attributes.
#[repr(C)]
pub struct posix_spawnattr_t {
    _opaque: [u8; 64],
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
    _file_actions: *const posix_spawn_file_actions_t,
    _attrp: *const posix_spawnattr_t,
    _argv: *const *const c_char,
    _envp: *const *const c_char,
) -> c_int {
    if path.is_null() {
        return EINVAL;
    }

    // Convert path to Rust str
    let path_str = unsafe {
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
    let procmgr_ep = match crate::registry::lookup_service("procmgr:spawn") {
        Some(ep) => ep,
        None => return ENOSYS,
    };

    let notify_endpoint = {
        let mut state = WAIT_STATE.lock();
        if state.notify_endpoint == 0 {
            let proc_cap = crate::boot::process_info().tokens[crate::boot::TOKEN_PROC_CAP];
            if proc_cap == 0 {
                return ENOSYS;
            }
            match crate::syscall::endpoint_create(proc_cap) {
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

    let mut msg = crate::types::Message::new(PROCMGR_SPAWN_LABEL, [0; 6], 4);
    msg.words[0] = payload.len();
    msg.words[1] = argc;
    msg.words[2] = 0; // reply endpoint (legacy)
    msg.words[3] = notify_endpoint;

    let mut reply = crate::types::Message::new(0, [0; 6], 0);
    match crate::ipc::call_with_payload(procmgr_ep, &msg, &payload, &mut reply) {
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
    file_actions: *const posix_spawn_file_actions_t,
    attrp: *const posix_spawnattr_t,
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
    let sh_path = b"/bin/sh\0".as_ptr() as *const c_char;
    let c_flag = b"-c\0".as_ptr() as *const c_char;
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
