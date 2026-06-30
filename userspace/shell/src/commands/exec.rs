//! Single-command spawn helpers.
//!
//! Canonical home of `spawn_process_with_argv_and_redirs` and related
//! helpers, moved from the old monolithic `commands.rs`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use cluu_wire::spawn::{FdInherit, FdRights, FdSource, SpawnEnvelope, SpawnError, ViewSource};
use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_IPC};
use libcluu::fs::client::VfsClient;
use libcluu::fd_table::FD_TABLE;
use libcluu::ipc::{
    call, RedirAction,
    TTY_FG_FLAG_FORWARD_CTRL_C, TTY_FG_FLAG_NOTIFY_CTRL_C, TTY_READ_LABEL,
    TTY_REGISTER_LABEL,
};
use libcluu::posix::tty::{
    get_lflag as tty_get_lflag, set_lflag as tty_set_lflag,
    TTY_LFLAG_ECHO, TTY_LFLAG_ICANON,
};
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{debug_print, Error, IpcFlags, Result};
use core::mem::size_of;
use procmgr_common::labels::SESSION_PROCMGR_SPAWN_LABEL;
use procmgr_common::wire::{FdInheritEntry, FdKind, SpawnReq, SpawnReply};

use crate::commands::builtins::registry::{CommandContext, ExecResult, ForegroundMode};

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

pub struct SpawnResult {
    pub procmgr_endpoint: usize,
    pub notify_endpoint: usize,
    pub status_word: usize,
    pub pid: usize,
}

/// UE17: bare-command PATH resolution + dispatch, plus path-with-slash
/// dispatch for inputs like `/bin/ls` that resolve through ext2 symlinks.
///
/// Called from `BuiltinRegistry::execute` when the first word didn't match
/// any builtin. For bare names, walks `$PATH` looking for an installed
/// container manifest. For paths-with-slashes, asks VFS to canonicalise
/// the path and recovers the bare image name from `/var/images/<name>/...`.
/// On hit, dispatches through `spawn_and_wait` and waits
/// for exit. On miss, returns NotHandled so the caller emits
/// "unsupported command".
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
    // Path-with-slash: try VFS realpath → bare image name. Bare names: $PATH lookup.
    let resolved_name = if name.contains('/') {
        match vfs.realpath(name) {
            Ok(canon) => match crate::path_lookup::image_name_from_canonical(&canon) {
                Some(bare) => bare,
                None => return Ok(ExecResult::NotHandled),
            },
            Err(_) => return Ok(ExecResult::NotHandled),
        }
    } else {
        match crate::path_lookup::resolve(name, &path_env, &vfs) {
            Some(n) => n,
            None => return Ok(ExecResult::NotHandled),
        }
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
/// `1` on internal error). Shared between pipeline single-stage paths
/// and UE17's PATH-dispatch fall-through.
///
/// Job control: a fresh pgid is created and the child attached to it before
/// `wait_for_exit_or_sigint` blocks. The TTY's fg-pgid for this session is
/// pointed at the child while it runs and restored to `shell_pgid` on
/// return. Without this, Ctrl-Z (TTY → SIGTSTP via PROCMGR_PG_SIGNAL on the
/// fg pgid) routed to whatever pgid the TTY had cached — usually the
/// shell — and the child never stopped. Pipeline path already does the
/// same dance in `pipeline.rs`; this brings parity for bare commands.
pub(crate) fn spawn_and_wait(
    stdout: usize,
    context: &mut CommandContext,
    name: &str,
    priority: usize,
    args: &[&str],
    fg_mode: ForegroundMode,
) -> Result<i32> {
    let spawn = spawn_process_with_argv(context, name, priority, args)?;
    let parsed = parse_status(spawn.status_word);
    let procmgr_ep = spawn.procmgr_endpoint;

    // Allocate a pgid and attach the child only on a successful spawn.
    let pgid = if parsed.is_ok() {
        let pg = libcluu::posix::jobs::pg_create(procmgr_ep).unwrap_or(0);
        if pg != 0 {
            let _ = libcluu::posix::jobs::pg_attach(procmgr_ep, pg, spawn.pid);
        }
        pg
    } else {
        0
    };

    let want_fg_swap = pgid != 0 && context.tty_stdout != 0 && context.session_id != 0;
    if want_fg_swap {
        let _ = libcluu::posix::jobs::tty_set_fg(
            context.tty_stdout,
            context.session_id,
            pgid,
        );
    }
    // In pts mode (cluuterm/VFS-backed stdin, no legacy tty service endpoint),
    // set the foreground pgid on fd 0 via PTS_SET_PGRP_LABEL so cluuterm knows
    // which process group to SIGINT when ^C arrives.
    let want_pts_fg_swap = pgid != 0 && context.tty_stdout == 0;
    if want_pts_fg_swap {
        let _ = libcluu::posix::termios::tcsetpgrp(0, pgid as i32);
    }

    let result = match parsed {
        Ok(()) => {
            let exit_code = wait_for_exit_or_sigint(
                procmgr_ep,
                context.tty_stdout,
                spawn.notify_endpoint,
                spawn.pid,
                stdout,
                fg_mode,
            )?;
            let signal_killed = exit_code > 128;
            if want_fg_swap && context.shell_pgid != 0 {
                let _ = libcluu::posix::jobs::tty_set_fg(
                    context.tty_stdout,
                    context.session_id,
                    context.shell_pgid,
                );
            }
            if want_pts_fg_swap && context.shell_pgid != 0 {
                let _ = libcluu::posix::termios::tcsetpgrp(0, context.shell_pgid as i32);
            }
            if signal_killed {
                crate::write_stdout(b"\x1b[2J\x1b[H");
            }
            Ok(0)
        }
        Err(err) => {
            let line = format!("spawn: {:?}\n", err);
            crate::write_stdout(line.as_bytes());
            Ok(1)
        }
    };

    result
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

    let mut env_pairs: Vec<(String, String)> = libcluu::posix::snapshot_env();
    for (k, v) in context.exported_pairs() {
        if let Some(idx) = env_pairs.iter().position(|(ek, _)| ek == &k) {
            env_pairs[idx].1 = v;
        } else {
            env_pairs.push((k, v));
        }
    }

    let fd_inherit: Vec<FdInherit> = {
        let table = FD_TABLE.lock();
        [0u32, 1u32, 2u32]
            .iter()
            .filter_map(|&fd| {
                let rights = match fd {
                    0 => FdRights::READ_ONLY,
                    _ => FdRights::WRITE_ONLY,
                };
                table.get(fd as i32).map(|e| FdInherit {
                    child_fd: fd,
                    source: FdSource::VfsFd {
                        vfs_client_id: e.client_id as u64,
                        vfs_remote_fd: e.remote_fd.unwrap_or(0) as u32,
                    },
                    rights,
                })
            })
            .collect()
    };
    let argv: Vec<String> = args.iter().map(|s| String::from(*s)).collect();
    let env: Vec<(String, String)> = env_pairs
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;

    if context.session_id != 0 {
        let ep_name = alloc::format!("session-procmgr:spawn:{}", context.session_id);
        if let Some(session_ep) = libcluu::registry::lookup_service(&ep_name) {
            let _ = debug_print(&format!(
                "shell: session spawn name={} sid={} ep={}",
                name, context.session_id, session_ep
            ));

            let fd_entries: Vec<FdInheritEntry> = fd_inherit
                .iter()
                .map(|fi| {
                    let (cap, rfd) = match &fi.source {
                        FdSource::VfsFd { vfs_client_id, vfs_remote_fd } => {
                            (*vfs_client_id, *vfs_remote_fd)
                        }
                        _ => (0, 0),
                    };
                    FdInheritEntry {
                        fd: fi.child_fd as i32,
                        kind: FdKind::Pts,
                        cap_token: cap,
                        parent_rfd: rfd,
                    }
                })
                .collect();

            let image_path = if name.contains('/') {
                String::from(name)
            } else {
                alloc::format!("/bin/{}", name)
            };
            let cwd = libcluu::posix::current_dir_string();
            let mut full_argv = alloc::vec![alloc::string::String::from(image_path.rsplit('/').next().unwrap_or(&image_path))];
            full_argv.extend(argv.iter().cloned());
            let spawn_req = SpawnReq {
                image_path,
                argv: full_argv,
                envp: env.clone(),
                cwd,
                fd_inherit: fd_entries,
                notify: Some(notify_endpoint as u64),
            };
            let payload = postcard::to_allocvec(&spawn_req)
                .map_err(|_| Error::InvalidState)?;
            let msg = Message::new(
                SESSION_PROCMGR_SPAWN_LABEL,
                [payload.len(), 0, 0, 0, 0, 0],
                0,
            );
            let mut reply_buf = [0u8; 512];
            let (reply_msg, reply_len) = libcluu::ipc::call_with_reply_buf(
                session_ep, &msg, &payload, &mut reply_buf,
            )?;
            let hdr = size_of::<Message>();
            let reply_bytes = &reply_buf[hdr..hdr + reply_len];
            let reply: SpawnReply = postcard::from_bytes(reply_bytes)
                .map_err(|_| Error::InvalidState)?;
            let _ = debug_print(&format!(
                "shell: session spawn done pid={}", reply.pid
            ));
            return Ok(SpawnResult {
                procmgr_endpoint: session_ep,
                notify_endpoint,
                status_word: 0,
                pid: reply.pid as usize,
            });
        }
    }

    let image = String::from(name);
    let envelope = SpawnEnvelope {
        image,
        args: argv,
        env,
        view: ViewSource::Derive(libcluu::token(TOKEN_EXTRA_0) as u64),
        fd_inherit,
        session: None,
        notify: Some(notify_endpoint as u64),
    };
    let _ = debug_print(&format!(
        "shell: spawn begin name={} ep={} notify={}",
        name, procmgr_endpoint, notify_endpoint
    ));
    let reply = libcluu::spawn::spawn(envelope).map_err(|e| match e {
        SpawnError::ImageNotFound => Error::NotFound,
        SpawnError::PermissionDenied => Error::PermissionDenied,
        SpawnError::OutOfMemory => Error::OutOfMemory,
        SpawnError::ViewDeriveDenied => Error::PermissionDenied,
        SpawnError::SessionRevoked => Error::PermissionDenied,
        SpawnError::NotifyTokenInvalid => Error::InvalidParameter,
        SpawnError::FdInheritDeniedAt(_) => Error::PermissionDenied,
        SpawnError::ManifestInvalid(_) => Error::InvalidState,
        SpawnError::Internal(_) => Error::InvalidState,
    })?;
    let _ = debug_print(&format!(
        "shell: spawn done pid={} child_thread_token=0x{:x}",
        reply.pid, reply.child_thread_token
    ));
    Ok(SpawnResult {
        procmgr_endpoint,
        notify_endpoint,
        status_word: 0,
        pid: reply.pid as usize,
    })
}

pub(crate) fn wait_for_exit_or_sigint(
    procmgr_endpoint: usize,
    tty_endpoint: usize,
    notify_endpoint: usize,
    child_pid: usize,
    stdout: usize,
    mode: ForegroundMode,
) -> Result<i32> {
    let mut ctrl_c_notify_endpoint = 0usize;
    let mut ctrl_c_flags = TTY_FG_FLAG_FORWARD_CTRL_C;
    if mode == ForegroundMode::SignalOnCtrlC {
        ctrl_c_notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
        ctrl_c_flags = TTY_FG_FLAG_NOTIFY_CTRL_C | TTY_FG_FLAG_FORWARD_CTRL_C;
    }

    // Unified PTS_* path: foreground routing is handled service-side via
    // PTS_SET_PGRP_LABEL (already set by tty_set_fg in spawn_and_wait).
    // No legacy TTY_REGISTER_LABEL forward needed.
    // Unified PTS_* path: termios (ECHO, ICANON) is managed service-side via
    // PTS_GET_TERMIOS_LABEL / PTS_SET_TERMIOS_LABEL. Shell no longer toggles
    // lflag directly — the legacy TTY_CTL_LABEL path is dead code.
    let _lflag_switched = false;

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
                    crate::write_stdout(line.as_bytes());
                } else if exit_code != 0 {
                    let line = format!("Exited with status {}\n", exit_code);
                    crate::write_stdout(line.as_bytes());
                }
                break Ok(exit_code);
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
            crate::write_stdout(line.as_bytes());
        }
    };

    // Unified PTS_* path: foreground routing restored service-side via
    // PTS_SET_PGRP_LABEL (already handled by tty_set_fg restore in spawn_and_wait).
    // No legacy TTY_CTL_LABEL lflag restore or TTY_REGISTER_LABEL fg-restore needed.
    let _ = debug_print("shell: wait_for_exit_or_sigint return");
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
