//! Single-command spawn helpers.
//!
//! Canonical home of `spawn_process_with_argv_and_redirs` and related
//! helpers, moved from the old monolithic `commands.rs`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libcluu::boot::process_info;
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{
    build_container_run_payload_full, call, call_with_payload, send_with_payload,
    RedirAction, PROCMGR_CONTAINER_RUN_LABEL,
    TTY_FG_FLAG_FORWARD_CTRL_C, TTY_FG_FLAG_NOTIFY_CTRL_C, TTY_READ_LABEL,
    TTY_REGISTER_LABEL, TTY_WRITE_LABEL,
};
use libcluu::posix::tty::{
    get_lflag as tty_get_lflag, set_lflag as tty_set_lflag,
    TTY_LFLAG_ECHO, TTY_LFLAG_ICANON,
};
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{debug_print, Error, IpcFlags, Result, TOKEN_IPC};
use core::mem::size_of;

use crate::commands::builtins::registry::{CommandContext, ExecResult, ForegroundMode, JobState};

const PROCMGR_KILL_LABEL: u32 = 3;
const SIGINT: usize = 2;
const DEFAULT_PRIORITY: usize = 200;
const TTY_LFLAG_DEFAULT: usize = TTY_LFLAG_ICANON | TTY_LFLAG_ECHO;

pub(crate) fn infer_foreground_mode(path: &str) -> ForegroundMode {
    let path = path.trim();
    if path.ends_with("/mp")
        || path.ends_with("/micropython")
        || path == "mp"
        || path == "micropython"
    {
        return ForegroundMode::PassCtrlCToChild;
    }
    ForegroundMode::SignalOnCtrlC
}

pub(crate) fn parse_spawn_args(args: &[String]) -> Option<(String, usize, ForegroundMode, Vec<String>)> {
    if args.is_empty() {
        return None;
    }
    let mut idx = 0usize;
    let mut mode = ForegroundMode::SignalOnCtrlC;
    let mut mode_explicit = false;
    while idx < args.len() {
        match args[idx].as_str() {
            "-i" | "--interactive" => {
                mode = ForegroundMode::PassCtrlCToChild;
                mode_explicit = true;
                idx += 1;
            }
            "-s" | "--signal" => {
                mode = ForegroundMode::SignalOnCtrlC;
                mode_explicit = true;
                idx += 1;
            }
            _ => break,
        }
    }
    let path = args.get(idx)?.clone();
    idx += 1;

    // Priority: if the next token parses as usize, consume it as priority. Else
    // leave it for argv. This preserves backward compat (`spawn foo 5`) while
    // allowing `spawn foo --help` to pass `--help` as argv[1].
    let priority = match args.get(idx).and_then(|v| v.parse::<usize>().ok()) {
        Some(p) => {
            idx += 1;
            p
        }
        None => DEFAULT_PRIORITY,
    };

    let argv_tail: Vec<String> = args[idx..].to_vec();

    if !mode_explicit {
        mode = infer_foreground_mode(path.as_str());
    }
    Some((path, priority, mode, argv_tail))
}

pub struct SpawnResult {
    pub procmgr_endpoint: usize,
    pub notify_endpoint: usize,
    pub status_word: usize,
    pub pid: usize,
    pub stdin_endpoint: usize,
}

/// UE17: bare-command PATH resolution + dispatch.
///
/// Called from `BuiltinRegistry::execute` when the first word didn't
/// match any builtin and isn't a literal path (no `/` in `name`). On
/// hit (i.e. `/var/images/<name>/manifest.toml` exists), dispatch the
/// binary through the same code SpawnBuiltin uses and wait for exit.
/// On miss, return NotHandled so the caller emits "unsupported command".
pub(crate) fn try_path_dispatch(
    stdout: usize,
    context: &mut CommandContext,
    name: &str,
    args: &[String],
) -> Result<ExecResult> {
    let path_env = read_path_env();
    let vfs_endpoint = match libcluu::registry::subscribe_output("vfs", "main") {
        Ok(ep) => ep,
        Err(_) => return Ok(ExecResult::NotHandled),
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(v) => v,
        Err(_) => return Ok(ExecResult::NotHandled),
    };
    let Some(resolved_name) = crate::path_lookup::resolve(name, &path_env, &vfs) else {
        return Ok(ExecResult::NotHandled);
    };
    let _ = debug_print(&format!(
        "shell: PATH resolved '{}' -> /var/images/{}",
        name, resolved_name
    ));
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let fg_mode = infer_foreground_mode(resolved_name.as_str());
    let status = spawn_and_wait(
        stdout,
        context,
        resolved_name.as_str(),
        DEFAULT_PRIORITY,
        &arg_refs,
        fg_mode,
    )?;
    context.set_last_status(status);
    Ok(ExecResult::Handled)
}

/// Read $PATH from the process env (envelope-resolved at session-login,
/// optionally overridden by `export PATH=...`). Falls back to a paranoid
/// `/bin:/usr/bin` default if PATH is unset or empty so PATH lookup at
/// least works for the standard installed images.
fn read_path_env() -> String {
    for (k, v) in libcluu::posix::snapshot_env() {
        if k == "PATH" {
            return v;
        }
    }
    String::from("/bin:/usr/bin")
}

/// Spawn `name` with `args`, wait for exit, return the exit code (or
/// `1` on internal error). Shared between `SpawnBuiltin::run` and
/// UE17's PATH-dispatch fall-through.
pub(crate) fn spawn_and_wait(
    stdout: usize,
    context: &mut CommandContext,
    name: &str,
    priority: usize,
    args: &[&str],
    fg_mode: ForegroundMode,
) -> Result<i32> {
    let spawn = spawn_process_with_argv(context, name, priority, args)?;
    match parse_status(spawn.status_word) {
        Ok(()) => {
            wait_for_exit_or_sigint(
                spawn.procmgr_endpoint,
                stdout,
                spawn.notify_endpoint,
                spawn.stdin_endpoint,
                spawn.pid,
                stdout,
                fg_mode,
            )?;
            // wait_for_exit_or_sigint doesn't surface the child's exit
            // code; we treat reaching here as success (status=0). Tighter
            // exit-code threading is a follow-up if `$?` plumbing
            // requires it.
            Ok(0)
        }
        Err(err) => {
            let line = format!("spawn: {:?}\n", err);
            let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
            Ok(1)
        }
    }
}

/// Build a `PROCMGR_CONTAINER_RUN_LABEL` payload of `name + CWD trailer`.
///
/// Procmgr reads the container image name from the start of the payload and
/// strips the CWD trailer (last 8 bytes + cwd_len) before slicing argv/FDAC,
/// so prepending the name and appending the trailer is safe even when the
pub(crate) fn spawn_process(context: &mut CommandContext, name: &str, priority: usize) -> Result<SpawnResult> {
    spawn_process_with_argv(context, name, priority, &[])
}

pub(crate) fn spawn_process_with_argv(
    context: &mut CommandContext,
    name: &str,
    _priority: usize,
    args: &[&str],
) -> Result<SpawnResult> {
    spawn_process_with_argv_and_redirs(context, name, _priority, args, &[])
}

pub fn spawn_process_with_argv_and_redirs(
    context: &mut CommandContext,
    name: &str,
    _priority: usize,
    args: &[&str],
    redirs: &[RedirAction],
) -> Result<SpawnResult> {
    let procmgr_endpoint = context.procmgr_spawn_endpoint()?;

    // Build the child's env: start from the shell's own (envelope-resolved at
    // session-login) env, then overlay any vars marked `export` in this
    // shell. Bash semantics: shell-local `set X=v` does NOT propagate; only
    // `export X` (or a var that was already inherited as exported) does.
    let mut env_pairs: Vec<(String, String)> = libcluu::posix::snapshot_env();
    for (k, v) in context.exported_pairs() {
        if let Some(idx) = env_pairs.iter().position(|(ek, _)| ek == &k) {
            env_pairs[idx].1 = v;
        } else {
            env_pairs.push((k, v));
        }
    }
    let env_refs: Vec<(&str, &str)> = env_pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let (payload, _argc, fdac_offset) =
        build_container_run_payload_full(name, args, &[], redirs, &env_refs);
    let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
    let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3);
    msg.words[0] = payload.len();
    msg.words[1] = notify_endpoint;
    msg.words[2] = fdac_offset;
    let mut reply = Message::new(0, [0; 6], 0);
    let _ = debug_print(&format!(
        "shell: container run begin name={} ep={} notify={}",
        name, procmgr_endpoint, notify_endpoint
    ));
    call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;
    let _ = debug_print(&format!(
        "shell: container run done status={} pid={} stdin={}",
        reply.words[0], reply.words[1], reply.words[4]
    ));
    Ok(SpawnResult {
        procmgr_endpoint,
        notify_endpoint,
        status_word: reply.words[0],
        pid: reply.words[1],
        stdin_endpoint: reply.words[4],
    })
}

pub(crate) fn wait_for_exit_or_sigint(
    procmgr_endpoint: usize,
    tty_endpoint: usize,
    notify_endpoint: usize,
    child_stdin_endpoint: usize,
    child_pid: usize,
    stdout: usize,
    mode: ForegroundMode,
) -> Result<()> {
    if child_stdin_endpoint == 0 {
        let _ = send_with_payload(
            stdout,
            TTY_WRITE_LABEL,
            b"spawn: invalid child stdin route\n",
        );
        return Err(Error::InvalidState);
    }

    let mut ctrl_c_notify_endpoint = 0usize;
    let mut ctrl_c_flags = TTY_FG_FLAG_FORWARD_CTRL_C;
    if mode == ForegroundMode::SignalOnCtrlC {
        ctrl_c_notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
        ctrl_c_flags = TTY_FG_FLAG_NOTIFY_CTRL_C | TTY_FG_FLAG_FORWARD_CTRL_C;
    }

    // Transfer tty foreground routing to the child while it runs.
    set_tty_foreground(
        tty_endpoint,
        child_stdin_endpoint,
        ctrl_c_notify_endpoint,
        ctrl_c_flags,
    )?;
    let saved_lflag = match tty_get_lflag(tty_endpoint) {
        Ok(lflag) => lflag,
        Err(err) => {
            let _ = debug_print(&format!("shell: tty_get_lflag failed {:?}", err));
            TTY_LFLAG_DEFAULT
        }
    };
    let mut lflag_switched = false;
    if mode == ForegroundMode::PassCtrlCToChild {
        let target_lflag = saved_lflag & !(TTY_LFLAG_ECHO | TTY_LFLAG_ICANON);
        match tty_set_lflag(tty_endpoint, target_lflag) {
            Ok(()) => lflag_switched = true,
            Err(err) => {
                let _ = debug_print(&format!("shell: tty_set_lflag(raw) failed {:?}", err));
            }
        }
    }

    let mut buf = [0u8; 256];
    let tokens = [notify_endpoint, ctrl_c_notify_endpoint];
    let active_tokens = if ctrl_c_notify_endpoint != 0 {
        &tokens[..2]
    } else {
        &tokens[..1]
    };

    let mut result = loop {
        let (index, len) = match syscall::ipc_recv_any(active_tokens, &mut buf, u64::MAX) {
            Ok(v) => v,
            Err(Error::WouldBlock) => continue,
            Err(err) => break Err(err),
        };
        let Some((msg, payload)) = parse_ipc_message(&buf[..len]) else {
            continue;
        };
        if index == 0 {
            // Exit notification from procmgr.
            if msg.tag.words >= 2 {
                let exit_code = msg.words[1] as i32;
                if exit_code > 128 {
                    let sig = exit_code - 128;
                    let sig_name = match sig {
                        4 => "Illegal instruction",
                        6 => "Aborted",
                        8 => "Floating point exception",
                        9 => "Killed",
                        11 => "Segmentation fault",
                        _ => "Signal",
                    };
                    let line = format!("{} (signal {})\n", sig_name, sig);
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
                } else if exit_code != 0 {
                    let line = format!("Exited with status {}\n", exit_code);
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
                }
                break Ok(());
            }
            continue;
        }

        // Ctrl-C notification from tty while child is foreground.
        if msg.tag.label != TTY_READ_LABEL {
            continue;
        }
        if payload.contains(&0x03) {
            let _ = signal_process(procmgr_endpoint, child_pid, SIGINT);
            let line = format!("spawn: SIGINT pid={}\n", child_pid);
            let _ = debug_print(line.trim_end());
            let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
        }
    };

    // Restore normal shell stdin routing once the foreground child finishes.
    if lflag_switched {
        if let Err(err) = tty_set_lflag(tty_endpoint, saved_lflag) {
            let _ = debug_print(&format!("shell: tty_set_lflag(restore) failed {:?}", err));
            if result.is_ok() {
                result = Err(err);
            }
        }
    }
    if let Err(err) = set_tty_foreground(tty_endpoint, 0, 0, TTY_FG_FLAG_FORWARD_CTRL_C) {
        let _ = debug_print(&format!("shell: tty foreground restore failed {:?}", err));
        if result.is_ok() {
            result = Err(err);
        }
    }
    result
}

pub(crate) fn set_tty_foreground(
    tty_endpoint: usize,
    foreground_endpoint: usize,
    ctrl_c_notify_endpoint: usize,
    flags: usize,
) -> Result<()> {
    let mut msg = Message::new(TTY_REGISTER_LABEL, [0; 6], 3);
    msg.words[0] = foreground_endpoint;
    msg.words[1] = ctrl_c_notify_endpoint;
    msg.words[2] = flags;
    call(tty_endpoint, &mut msg, IpcFlags::empty())?;
    Ok(())
}

/// Poll background job notify endpoints and emit async completion markers.
pub fn poll_background_jobs(stdout: usize, context: &mut CommandContext) -> Result<()> {
    let mut finished: Vec<(usize, i32)> = Vec::new();
    let mut buf = [0u8; 128];

    for (pid, job) in &context.bg_jobs {
        match syscall::ipc_recv_nonblocking(job.notify_endpoint, &mut buf) {
            Ok(len) => {
                if let Some((msg, _payload)) = parse_ipc_message(&buf[..len]) {
                    if msg.tag.label == 1 && msg.tag.words >= 2 {
                        finished.push((*pid, msg.words[1] as i32));
                    }
                }
            }
            Err(Error::WouldBlock) => {}
            Err(_) => {}
        }
    }

    for (pid, code) in finished {
        context.remove_bg_job(pid);
        let line = format!("shell: bg done pid={} code={}\n", pid, code);
        let _ = debug_print(line.trim_end());
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
    }
    Ok(())
}

pub(crate) fn parse_ipc_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    if buf.len() < size_of::<Message>() {
        return None;
    }
    let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
    let mut payload_len = msg.words[0];
    let header = size_of::<Message>();
    if header + payload_len > buf.len() {
        payload_len = 0;
    }
    Some((msg, &buf[header..header + payload_len]))
}

pub(crate) fn resolve_job_pid(context: &CommandContext, arg: Option<&String>) -> Option<usize> {
    let Some(token) = arg else {
        return context.latest_bg_pid();
    };
    let raw = token.strip_prefix('%').unwrap_or(token.as_str());
    raw.parse::<usize>().ok()
}

pub(crate) fn ensure_bg_job_state(context: &mut CommandContext, pid: usize, state: JobState) -> Result<()> {
    if context.set_bg_job_state(pid, state) {
        return Ok(());
    }
    let line = format!("shell: invariant violation missing job pid={}", pid);
    let _ = debug_print(line.as_str());
    Err(Error::InvalidState)
}

pub(crate) fn parse_status(raw: usize) -> Result<()> {
    let signed = raw as isize;
    if signed < 0 {
        return Err(Error::from_errno(signed));
    }
    Ok(())
}

pub(crate) fn signal_process(procmgr_endpoint: usize, pid: usize, signal: usize) -> Result<()> {
    let mut req = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
    req.words[0] = pid;
    req.words[1] = signal;
    call(procmgr_endpoint, &mut req, IpcFlags::empty())?;
    parse_status(req.words[0])
}
