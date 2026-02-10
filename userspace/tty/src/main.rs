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

use context::TtyContext;
use libcluu::ipc::{
    extract_reply_token, reply, KBD_EVENT_LABEL, TTY_CTL_LABEL, TTY_READ_LABEL,
    TTY_READ_REQUEST_LABEL, TTY_REGISTER_LABEL, TTY_WRITE_LABEL, TTY_WRITE_SYNC_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{yield_cpu, Error, Result};
use line_discipline::{EchoAction, LineDiscipline};
use protocol::{decode_kbd_event, parse_message};

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
fn run() -> Result<()> {
    let mut ctx = TtyContext::new()?;
    let mut discipline = LineDiscipline::new();

    let mut buf = [0u8; 256];
    loop {
        ctx.request_subscriptions();

        let tokens = [ctx.endpoint, ctx.registry_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    if index == 1 {
                        ctx.handle_registry_event(&msg, payload);
                        continue;
                    }

                    match msg.tag.label {
                        KBD_EVENT_LABEL => {
                            if let Some(event) = decode_kbd_event(&msg) {
                                handle_key(&mut ctx, &mut discipline, event.ascii, event.extended);
                            }
                        }
                        TTY_READ_REQUEST_LABEL => {
                            // A process called read(0, buf, n) — enqueue the request
                            if let Some(reply_token) = extract_reply_token(&msg) {
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
                            // Synchronous write: forward to console, then reply
                            // If console not ready, defer reply until flush
                            if let Some(reply_token) = extract_reply_token(&msg) {
                                if ctx.forward_to_console_sync(payload, reply_token) {
                                    // Output sent immediately, reply now
                                    let reply_msg = Message::new(TTY_WRITE_SYNC_LABEL, [0; 6], 0);
                                    let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                                }
                                // else: reply deferred until console is ready
                            } else {
                                // No reply token - treat as async write
                                ctx.forward_to_console(payload);
                            }
                        }
                        TTY_CTL_LABEL => {
                            // Terminal control: get/set mode
                            if let Some(reply_token) = extract_reply_token(&msg) {
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
                            // Legacy path: a process can register stdin directly.
                            ctx.shell_stdin = msg.words[0];
                        }
                        _ => {}
                    }
                }
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

/// Apply line discipline to a character and route echo/line output.
fn handle_key(ctx: &mut TtyContext, discipline: &mut LineDiscipline, ch: u8, extended: u8) {
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
            apply_effect(ctx, effect);
            return;
        }
    };

    // Feed each byte of the escape sequence through the discipline
    for &b in bytes {
        let effect = discipline.handle_byte(b);
        apply_effect(ctx, effect);
    }
}

/// Apply a line discipline effect: echo and deliver line/raw data.
fn apply_effect(ctx: &mut TtyContext, effect: line_discipline::LineEffect) {
    match effect.echo {
        EchoAction::None => {}
        EchoAction::Bytes(bytes) => ctx.forward_to_console(bytes),
        EchoAction::Byte(byte) => ctx.forward_to_console(&[byte]),
    }

    if let Some(raw) = effect.raw_byte {
        ctx.input_queue.push_back(raw);
        ctx.try_satisfy_reads();
    }

    if let Some(line) = effect.line_ready {
        deliver_line(ctx, &line);
    }
}

/// Deliver a completed line to pending readers or the shell.
fn deliver_line(ctx: &mut TtyContext, line: &[u8]) {
    // If there are pending read() requests, satisfy them first
    if !ctx.pending_reads.is_empty() {
        for &b in line {
            ctx.input_queue.push_back(b);
        }
        ctx.try_satisfy_reads();
        return;
    }

    // Otherwise push to the shell (backward compat)
    if ctx.shell_stdin == 0 {
        return;
    }
    let _ = libcluu::ipc::send_with_payload(ctx.shell_stdin, TTY_READ_LABEL, line);
}
