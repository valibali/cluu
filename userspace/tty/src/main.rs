#![no_std]
#![no_main]

//! TTY service for CLUU.
//!
//! The tty provides line discipline, echoes to the console, and delivers
//! stdin to processes (shell today, multiple sessions in the future).

extern crate alloc;

mod context;
mod protocol;

use context::{TtyContext, TtyMode, LoginState};
use cluu_wire::pts::{
    PTS_GET_PGRP_LABEL, PTS_GET_TERMIOS_LABEL, PTS_POLL_LABEL, PTS_READ_LABEL,
    PTS_SET_PGRP_LABEL, PTS_SET_TERMIOS_LABEL, PTS_WRITE_LABEL,
    GetPgrpReply, GetTermiosReply, PollReply, PtsErr,
    SetPgrpReply, SetTermiosReply,
};
use libcluu::ipc::{
    extract_reply_id, reply, CONSOLE_CREDIT_REFILL_LABEL, KBD_EVENT_LABEL,
    TTY_REGISTER_LABEL, TTY_WRITE_SYNC_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{yield_cpu, Error, Result};
use libcluu::tty_core::{EchoAction, LineDiscipline, LineEffect, TermMode};
use protocol::{decode_kbd_event, parse_message};

/// POSIX signal numbers used for job-control keystroke routing.
const SIGINT: i32 = 2;
const SIGTSTP: i32 = 20;

#[no_mangle]
/// Kernel entrypoint for the tty service.
///
/// Delegates to `run` to keep the ABI surface small.
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

/// Main service loop: handle registry wiring, input discipline, and output routing.
///
/// Two-stage receive on each iteration:
/// 1. Block on recv_any waiting for the next message.
/// 2. After processing, drain any back-to-back pending messages with
///    timeout=0 (nonblocking).  This batches multiple keystrokes into
///    one console flush — without it, fast typing produces N separate
///    CONSOLE_WRITE messages, each forcing a full render pipeline in
///    the console.  Sustained ~20 msg/sec render rate easily backs
///    up the queue and looks like a freeze to the user.
fn run() -> Result<()> {
    let mut ctx = TtyContext::new()?;
    let mut discipline = LineDiscipline::new();

    let mut buf = [0u8; 4096];
    loop {
        ctx.request_subscriptions();

        let tokens = [ctx.endpoint, ctx.registry_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                handle_one_message(index, &buf[..len], &mut ctx, &mut discipline);
                // Nonblocking drain of any back-to-back messages so a
                // burst of keystrokes batches into one console flush.
                loop {
                    match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, 0) {
                        Ok((idx, n)) => {
                            handle_one_message(idx, &buf[..n], &mut ctx, &mut discipline);
                        }
                        Err(_) => break,
                    }
                }
                ctx.flush_pending_console();
            }
            Err(Error::WouldBlock) => {
                let _ = yield_cpu();
            }
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    }
}

/// Dispatch a single message received from `ctx.endpoint` (index 0) or
/// `ctx.registry_endpoint` (index 1).  All console output is queued via
/// `ctx.forward_to_console` (which buffers into `pending_console_output`)
/// rather than sent immediately — the caller is expected to flush after a
/// burst.
fn handle_one_message(
    index: usize,
    msg_buf: &[u8],
    ctx: &mut TtyContext,
    discipline: &mut LineDiscipline,
) {
    let Some((msg, payload)) = parse_message(msg_buf) else {
        return;
    };
    if index == 1 {
        ctx.handle_registry_event(&msg, payload);
        return;
    }

    match msg.tag.label {
        KBD_EVENT_LABEL => {
            if let Some(event) = decode_kbd_event(&msg) {
                handle_key(ctx, discipline, event.ascii, event.extended);
            }
        }
        PTS_READ_LABEL => {
            // PTS_READ_LABEL: process called read(0, buf, n) — enqueue the request.
            // Request is postcard-serialized ReadRequest; words[0] = payload len.
            if let Some(reply_token) = extract_reply_id(&msg) {
                let max_bytes: usize = if payload.len() >= 4 {
                    postcard::from_bytes::<cluu_wire::pts::ReadRequest>(payload)
                        .map(|r| r.max_bytes as usize)
                        .unwrap_or(0)
                } else {
                    msg.words[0] // legacy fallback for pre-spec2 callers
                };
                ctx.pending_reads.push_back(context::PendingRead {
                    reply_token,
                    max_bytes,
                });
                ctx.try_satisfy_reads();
            }
        }
        PTS_WRITE_LABEL => {
            // Shell wrote output via PTS_WRITE — forward to console framebuffer.
            ctx.forward_to_console(payload);
        }
        TTY_WRITE_SYNC_LABEL => {
            // Forward as async, reply immediately so caller never blocks.
            ctx.forward_to_console_sync(payload);
            if let Some(reply_token) = extract_reply_id(&msg) {
                let reply_msg = Message::new(TTY_WRITE_SYNC_LABEL, [0; 6], 0);
                let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }
        CONSOLE_CREDIT_REFILL_LABEL => {
            let refill_amount = msg.words[0];
            ctx.handle_credit_refill(refill_amount);
        }
        PTS_GET_TERMIOS_LABEL => {
            // Reply with current termios via postcard.
            if let Some(reply_token) = extract_reply_id(&msg) {
                let t = discipline.termios;
                let value: GetTermiosReply = t;
                let bytes = postcard::to_allocvec(&value).unwrap_or_default();
                let reply_msg = Message::new(PTS_GET_TERMIOS_LABEL, [0; 6], 0);
                let _ = libcluu::ipc::reply_with_payload(reply_token, &reply_msg, &bytes);
            }
        }
        PTS_SET_TERMIOS_LABEL => {
            // Deserialize SetTermiosRequest, apply to discipline.
            if let Some(reply_token) = extract_reply_id(&msg) {
                let result: core::result::Result<(), PtsErr> = if let Ok(req) =
                    postcard::from_bytes::<cluu_wire::pts::SetTermiosRequest>(payload)
                {
                    match discipline.set_termios(req.termios) {
                        Ok(()) => core::result::Result::Ok(()),
                        Err(_) => core::result::Result::Err(PtsErr::EinvalTermios),
                    }
                } else {
                    core::result::Result::Err(PtsErr::EinvalTermios)
                };
                let value: SetTermiosReply = result;
                let bytes = postcard::to_allocvec(&value).unwrap_or_default();
                let reply_msg = Message::new(PTS_SET_TERMIOS_LABEL, [0; 6], 0);
                let _ = libcluu::ipc::reply_with_payload(reply_token, &reply_msg, &bytes);
            }
        }
        TTY_REGISTER_LABEL => {
            // words=1 legacy: set active stdin route.
            // words>=3: set route + Ctrl-C notify endpoint + policy flags.
            let foreground_endpoint = msg.words[0];
            if ctx.mode != TtyMode::Terminal && foreground_endpoint != 0 {
                // Auto-login: enter Terminal mode (Path A — push wiring dropped).
                ctx.enter_terminal_mode();
            } else if msg.tag.words >= 3 {
                ctx.configure_foreground(
                    foreground_endpoint,
                    msg.words[1],
                    msg.words[2],
                );
            } else {
                ctx.configure_foreground(foreground_endpoint, 0, 0);
            }
            if foreground_endpoint == 0 {
                // Foreground returned to shell: force canonical+echo so
                // shell input cannot get stuck in child raw mode.
                discipline.set_mode(TermMode::default());
            }
            if let Some(reply_token) = extract_reply_id(&msg) {
                let reply_msg = Message::new(TTY_REGISTER_LABEL, [0; 6], 0);
                let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }
        PTS_POLL_LABEL => {
            // Readiness query from poll(): deserialize PollRequest, reply with PollReply.
            if let Some(reply_token) = extract_reply_id(&msg) {
                let has_data = !ctx.input_queue.is_empty();
                let ready = if has_data {
                    cluu_wire::pts::PollEvents::POLLIN
                } else {
                    cluu_wire::pts::PollEvents::empty()
                };
                let reply_val = PollReply { ready };
                let bytes = postcard::to_allocvec(&reply_val).unwrap_or_default();
                let reply_msg = Message::new(PTS_POLL_LABEL, [0; 6], 0);
                let _ = libcluu::ipc::reply_with_payload(reply_token, &reply_msg, &bytes);
            }
        }
        libcluu::ipc::PROCMGR_SESSION_DEATH_LABEL => {
            ctx.handle_session_death();
            discipline.set_mode(TermMode::default());
        }
        PTS_SET_PGRP_LABEL => {
            // Set the foreground pgid for a session via postcard SetPgrpRequest (i32).
            if let Some(reply_token) = extract_reply_id(&msg) {
                let pgid: i32 = if !payload.is_empty() {
                    postcard::from_bytes::<cluu_wire::pts::SetPgrpRequest>(payload)
                        .unwrap_or(0)
                } else {
                    msg.words[1] as i32 // legacy fallback: words[1]=pgid
                };
                let session = ctx.session_id();
                let pgid_usize = pgid as usize;
                if pgid == 0 {
                    ctx.fg_pgid_per_session.remove(&session);
                } else {
                    ctx.fg_pgid_per_session.insert(session, pgid_usize);
                }
                let value: SetPgrpReply = Ok(());
                let bytes = postcard::to_allocvec(&value).unwrap_or_default();
                let reply_msg = Message::new(PTS_SET_PGRP_LABEL, [0; 6], 0);
                let _ = libcluu::ipc::reply_with_payload(reply_token, &reply_msg, &bytes);
            }
        }
        PTS_GET_PGRP_LABEL => {
            // Query foreground pgid for a session, reply via postcard GetPgrpReply.
            if let Some(reply_token) = extract_reply_id(&msg) {
                let session = ctx.session_id();
                let pgid: GetPgrpReply =
                    ctx.fg_pgid_per_session.get(&session).copied().unwrap_or(0) as i32;
                let bytes = postcard::to_allocvec(&pgid).unwrap_or_default();
                let reply_msg = Message::new(PTS_GET_PGRP_LABEL, [0; 6], 0);
                let _ = libcluu::ipc::reply_with_payload(reply_token, &reply_msg, &bytes);
            }
        }
        _ => {}
    }
}

/// Apply line discipline to a character and route echo/line output.
fn handle_key(ctx: &mut TtyContext, discipline: &mut LineDiscipline, ch: u8, extended: u8) {
    if ctx.mode != TtyMode::Terminal {
        handle_login_key(ctx, ch, extended);
        return;
    }
    if let Some(bytes) = libcluu::tty_core::keymap::encode_extended(extended) {
        for &b in bytes {
            let effect = discipline.handle_byte(b);
            apply_effect(ctx, discipline, effect);
        }
    } else {
        let effect = discipline.handle_byte(ch);
        apply_effect(ctx, discipline, effect);
    }
}

fn handle_login_key(ctx: &mut TtyContext, ch: u8, extended: u8) {
    if extended != 0 { return; }

    match ctx.mode {
        TtyMode::Login(LoginState::Username) => {
            if ch == b'\r' || ch == b'\n' {
                if ctx.login_username.is_empty() {
                    ctx.write_to_console(b"\r\nlogin: ");
                    return;
                }
                ctx.mode = TtyMode::Login(LoginState::Password);
                ctx.write_to_console(b"\r\npassword: ");
            } else if ch == 0x7f || ch == 0x08 {
                if !ctx.login_username.is_empty() {
                    ctx.login_username.pop();
                    ctx.write_to_console(b"\x08 \x08");
                }
            } else if ch == 0x03 {
                ctx.login_username.clear();
                ctx.write_to_console(b"\r\nlogin: ");
            } else if ch >= 0x20 && ch < 0x7f {
                ctx.login_username.push(ch);
                ctx.write_to_console(&[ch]);
            }
        }
        TtyMode::Login(LoginState::Password) => {
            if ch == b'\r' || ch == b'\n' {
                ctx.write_to_console(b"\r\n");
                ctx.send_login_request();
            } else if ch == 0x7f || ch == 0x08 {
                if !ctx.login_password.is_empty() {
                    ctx.login_password.pop();
                }
            } else if ch == 0x03 {
                for b in ctx.login_password.iter_mut() { *b = 0; }
                ctx.login_password.clear();
                ctx.login_username.clear();
                ctx.mode = TtyMode::Login(LoginState::Username);
                ctx.write_to_console(b"\r\nlogin: ");
            } else if ch >= 0x20 && ch < 0x7f {
                ctx.login_password.push(ch);
            }
        }
        TtyMode::Login(LoginState::Authenticating) => {}
        TtyMode::Terminal => {}
    }
}

/// Apply a line discipline effect: echo and deliver line/raw data.
fn apply_effect(ctx: &mut TtyContext, _discipline: &mut LineDiscipline, effect: LineEffect) {
    match effect.echo {
        EchoAction::None => {}
        EchoAction::Bytes(bytes) => ctx.forward_to_console(bytes),
        EchoAction::Byte(byte) => ctx.forward_to_console(&[byte]),
        EchoAction::OwnedBytes(bytes) => ctx.forward_to_console(&bytes),
    }

    if let Some(raw) = effect.raw_byte {
        ctx.input_queue.push_back(raw);
        ctx.try_satisfy_reads();
    }

    // TAB completion through the shell required a recv loop on the shell's
    // stdin endpoint, which Path A retired. Re-wire later via fd-0-based
    // completion (separate spec). For now, just drop the tab event.
    let _ = effect.tab_request;

    if let Some(line) = effect.line_ready {
        let is_ctrl_c = line.len() == 1 && line[0] == 0x03;
        let is_ctrl_z = line.len() == 1 && line[0] == 0x1A;

        if is_ctrl_c {
            // Out-of-band Ctrl-C notify retired with the TTY_READ_LABEL push.
            // Job-control signal delivery via PROCMGR_PG_SIGNAL handles SIGINT.
            // Job-control: send SIGINT to the foreground pgid for this session.
            let session = ctx.session_id();
            if let Some(&pgid) = ctx.fg_pgid_per_session.get(&session) {
                if ctx.procmgr_main != 0 {
                    let _ = send_pg_signal(ctx.procmgr_main, pgid, SIGINT);
                }
            }
            // Do not deliver Ctrl-C byte to readers.
            return;
        }

        if is_ctrl_z {
            // Job-control: send SIGTSTP to the foreground pgid for this session.
            let session = ctx.session_id();
            if let Some(&pgid) = ctx.fg_pgid_per_session.get(&session) {
                if ctx.procmgr_main != 0 {
                    let _ = send_pg_signal(ctx.procmgr_main, pgid, SIGTSTP);
                }
            }
            // Do not forward Ctrl-Z to readers; it is consumed here.
            return;
        }

        deliver_line(ctx, &line);
    }
}

/// Send PROCMGR_PG_SIGNAL to deliver `signum` to all members of `pgid`.
///
/// Fire-and-forget: TTY does not wait for a reply.  The TTY service does not
/// have the `posix` libcluu feature enabled, so this is an inline send rather
/// than going through `libcluu::posix::jobs`.
fn send_pg_signal(procmgr_ep: usize, pgid: usize, signum: i32) -> Result<()> {
    let msg = Message::new(
        libcluu::ipc::PROCMGR_PG_SIGNAL_LABEL,
        [pgid, signum as usize, 0, 0, 0, 0],
        2,
    );
    libcluu::ipc::send(procmgr_ep, &msg, IpcFlags::empty())
}

/// Deliver a completed line by queueing it for the next PTS_READ.
///
/// Path A unification: the shell (and any other reader) opens /dev/ttyN
/// and calls POSIX read(0); VFS forwards the request here as
/// PTS_READ_LABEL. There is no longer a TTY_READ_LABEL push path —
/// bytes always sit in input_queue until a pending read drains them.
fn deliver_line(ctx: &mut TtyContext, line: &[u8]) {
    let _ = libcluu::debug_print(&alloc::format!(
        "tty: deliver_line len={} pending_reads={}",
        line.len(), ctx.pending_reads.len()
    ));
    for &b in line {
        ctx.input_queue.push_back(b);
    }
    ctx.try_satisfy_reads();
}
