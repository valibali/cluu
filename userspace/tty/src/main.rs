#![no_std]
#![no_main]

//! TTY service for CLUU.
//!
//! The tty provides line discipline, echoes to the console, and delivers
//! stdin to processes (shell today, multiple sessions in the future).

extern crate alloc;

mod context;
mod line_discipline;
mod protocol;

use context::{TtyContext, TtyMode, LoginState};
use libcluu::ipc::{
    extract_reply_id, reply, CONSOLE_CREDIT_REFILL_LABEL, KBD_EVENT_LABEL, TTY_CTL_LABEL,
    TTY_FG_FLAG_FORWARD_CTRL_C, TTY_GET_FG_LABEL, TTY_POLL_QUERY_LABEL, TTY_READ_LABEL,
    TTY_READ_REQUEST_LABEL, TTY_REGISTER_LABEL, TTY_SET_FG_LABEL, TTY_TAB_QUERY_LABEL,
    TTY_WRITE_LABEL, TTY_WRITE_SYNC_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{yield_cpu, Error, Result};
use line_discipline::{EchoAction, LineDiscipline};
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
        TTY_READ_REQUEST_LABEL => {
            // A process called read(0, buf, n) — enqueue the request
            if let Some(reply_token) = extract_reply_id(&msg) {
                let max_bytes = msg.words[0];
                ctx.pending_reads.push_back(context::PendingRead {
                    reply_token,
                    max_bytes,
                });
                ctx.try_satisfy_reads();
            }
        }
        TTY_WRITE_LABEL => {
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
        TTY_CTL_LABEL => {
            // Terminal control: get/set mode
            if let Some(reply_token) = extract_reply_id(&msg) {
                let subcmd = msg.words[0];
                match subcmd {
                    0 => {
                        // getattr: reply with current mode flags
                        let mode = &discipline.mode;
                        let lflag: usize = (if mode.canonical { 0x02 } else { 0 })
                            | (if mode.echo { 0x08 } else { 0 });
                        let reply_msg =
                            Message::new(TTY_CTL_LABEL, [0, 0, 0, 0, lflag, 0], 5);
                        let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                    1 => {
                        // setattr: update discipline mode
                        let lflag = msg.words[4];
                        let new_mode = line_discipline::TermMode {
                            canonical: (lflag & 0x02) != 0,
                            echo: (lflag & 0x08) != 0,
                        };
                        discipline.set_mode(new_mode);
                        let reply_msg = Message::new(TTY_CTL_LABEL, [0; 6], 0);
                        let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                    _ => {
                        let reply_msg = Message::new(TTY_CTL_LABEL, [0; 6], 0);
                        let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                }
            }
        }
        TTY_REGISTER_LABEL => {
            // words=1 legacy: set active stdin route.
            // words>=3: set route + Ctrl-C notify endpoint + policy flags.
            let foreground_endpoint = msg.words[0];
            if ctx.mode != TtyMode::Terminal && foreground_endpoint != 0 {
                // Auto-login: procmgr wired shell stdin directly.
                ctx.wire_shell_stdin(foreground_endpoint);
            } else if msg.tag.words >= 3 {
                ctx.configure_foreground(
                    foreground_endpoint,
                    msg.words[1],
                    msg.words[2],
                );
            } else {
                ctx.configure_foreground(
                    foreground_endpoint,
                    0,
                    TTY_FG_FLAG_FORWARD_CTRL_C,
                );
            }
            if foreground_endpoint == 0 {
                // Foreground returned to shell: force canonical+echo so
                // shell input cannot get stuck in child raw mode.
                discipline.set_mode(line_discipline::TermMode::default());
            }
            if let Some(reply_token) = extract_reply_id(&msg) {
                let reply_msg = Message::new(TTY_REGISTER_LABEL, [0; 6], 0);
                let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }
        TTY_POLL_QUERY_LABEL => {
            // Readiness query from poll(): reply with 1 if input available, 0 otherwise.
            if let Some(reply_token) = extract_reply_id(&msg) {
                let has_data = if ctx.input_queue.is_empty() {
                    0usize
                } else {
                    1usize
                };
                let reply_msg = Message::new(
                    TTY_POLL_QUERY_LABEL,
                    [has_data, 0, 0, 0, 0, 0],
                    1,
                );
                let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }
        libcluu::ipc::PROCMGR_SESSION_DEATH_LABEL => {
            ctx.handle_session_death();
            discipline.set_mode(line_discipline::TermMode::default());
        }
        TTY_SET_FG_LABEL => {
            // Set the foreground pgid for a session.
            // words[0] = session_id, words[1] = pgid.
            let session = msg.words[0];
            let pgid = msg.words[1];
            if pgid == 0 {
                ctx.fg_pgid_per_session.remove(&session);
            } else {
                ctx.fg_pgid_per_session.insert(session, pgid);
            }
            // Reply if caller used call semantics; ignore if fire-and-forget.
            if let Some(reply_token) = extract_reply_id(&msg) {
                let reply_msg = Message::new(TTY_SET_FG_LABEL, [0; 6], 0);
                let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }
        TTY_GET_FG_LABEL => {
            // Query the foreground pgid for a session.
            // words[0] = session_id. Reply: words[0]=0, words[1]=pgid.
            if let Some(reply_token) = extract_reply_id(&msg) {
                let session = msg.words[0];
                let pgid = ctx.fg_pgid_per_session.get(&session).copied().unwrap_or(0);
                let reply_msg = Message::new(TTY_GET_FG_LABEL, [0, pgid, 0, 0, 0, 0], 2);
                let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
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
    // Convert extended key codes to ANSI escape sequences before processing
    let bytes: &[u8] = match extended {
        1 => b"\x1b[A",  // KEY_UP
        2 => b"\x1b[B",  // KEY_DOWN
        3 => b"\x1b[D",  // KEY_LEFT  (D = left in ANSI)
        4 => b"\x1b[C",  // KEY_RIGHT (C = right in ANSI)
        5 => b"\x1b[H",  // KEY_HOME
        6 => b"\x1b[F",  // KEY_END
        7 => b"\x1b[3~", // KEY_DELETE
        _ => {
            // Normal ASCII byte
            let effect = discipline.handle_byte(ch);
            apply_effect(ctx, discipline, effect);
            return;
        }
    };

    // Feed each byte of the escape sequence through the discipline
    for &b in bytes {
        let effect = discipline.handle_byte(b);
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
fn apply_effect(ctx: &mut TtyContext, discipline: &mut line_discipline::LineDiscipline, effect: line_discipline::LineEffect) {
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

    if let Some(partial) = effect.tab_request {
        if let Some(completion) = compute_path_completion(&partial, ctx) {
            // Echo the completion bytes and append to the line buffer so the
            // user sees the suffix and the line ready-event will pick it up.
            ctx.forward_to_console(&completion);
            discipline.append_completion(&completion);
        }
    }

    if let Some(line) = effect.line_ready {
        let is_ctrl_c = line.len() == 1 && line[0] == 0x03;
        let is_ctrl_z = line.len() == 1 && line[0] == 0x1A;

        if is_ctrl_c {
            // Existing ctrl_c_notify path: out-of-band notification to foreground.
            if ctx.ctrl_c_notify != 0 {
                let _ = libcluu::ipc::send_with_payload(ctx.ctrl_c_notify, TTY_READ_LABEL, &[0x03]);
            }
            // Job-control: send SIGINT to the foreground pgid for this session.
            let session = ctx.session_id();
            if let Some(&pgid) = ctx.fg_pgid_per_session.get(&session) {
                if ctx.procmgr_main != 0 {
                    let _ = send_pg_signal(ctx.procmgr_main, pgid, SIGINT);
                }
            }
            if !ctx.forward_ctrl_c {
                return;
            }
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

/// Resolve a TAB press to a completion suffix.
///
/// Synchronously calls shell's stdin endpoint with the partial last-token
/// bytes. Shell does the readdir using its own VFS view + CWD (which TTY
/// can't see) and replies with the suffix. Returns `Some(bytes)` for a
/// completion, `None` for "no completion / shell unavailable / multiple
/// matches" — all silent failures.
///
/// 250 ms timeout: shell typically replies in well under a millisecond,
/// but if it's mid-pipeline we'd rather drop the TAB than freeze TTY.
fn compute_path_completion(partial: &[u8], ctx: &mut TtyContext) -> Option<alloc::vec::Vec<u8>> {
    let token_start = partial
        .iter()
        .rposition(|&b| b == b' ' || b == b'\t')
        .map(|p| p + 1)
        .unwrap_or(0);
    let token = &partial[token_start..];

    if ctx.shell_stdin == 0 {
        return None;
    }

    let mut req = Message::new(TTY_TAB_QUERY_LABEL, [0; 6], 1);
    req.words[0] = token.len();
    let header = req.as_bytes();
    let mut send_buf = alloc::vec::Vec::with_capacity(header.len() + token.len());
    send_buf.extend_from_slice(header);
    send_buf.extend_from_slice(token);

    let mut reply_buf = [0u8; 256];
    let bytes_recv = libcluu::syscall::ipc_call_timeout(
        ctx.shell_stdin,
        &send_buf,
        &mut reply_buf,
        250,
    )
    .ok()?;

    let (msg, payload) = libcluu::ipc::parse_message(&reply_buf[..bytes_recv])?;
    if msg.tag.label != TTY_TAB_QUERY_LABEL || payload.is_empty() {
        return None;
    }
    Some(payload.to_vec())
}

/// Deliver a completed line to pending readers or the shell.
fn deliver_line(ctx: &mut TtyContext, line: &[u8]) {
    // If there are pending read() requests, satisfy them first
    if !ctx.pending_reads.is_empty() {
        for &b in line {
            ctx.input_queue.push_back(b);
        }
        ctx.try_satisfy_reads();
        // If pending readers were stale and dropped, hand remaining bytes back
        // to shell delivery so input does not disappear after foreground exit.
        if ctx.pending_reads.is_empty() && ctx.shell_stdin != 0 && !ctx.input_queue.is_empty() {
            let buffered: alloc::vec::Vec<u8> = ctx.input_queue.drain(..).collect();
            ctx.deliver_shell_line(&buffered);
        }
        return;
    }

    // If shell stdin routing is disabled (foreground child handover),
    // keep bytes queued until a reader calls TTY_READ_REQUEST.
    if ctx.shell_stdin == 0 {
        for &b in line {
            ctx.input_queue.push_back(b);
        }
        ctx.try_satisfy_reads();
        return;
    }

    // Otherwise push to the shell (backward compat), with route repair.
    ctx.deliver_shell_line(line);
}
