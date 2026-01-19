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
    extract_reply_token, reply, KBD_EVENT_LABEL, TTY_READ_LABEL, TTY_REGISTER_LABEL,
    TTY_WRITE_LABEL, TTY_WRITE_SYNC_LABEL,
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
                                handle_key(&mut ctx, &mut discipline, event.ascii);
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
fn handle_key(ctx: &mut TtyContext, discipline: &mut LineDiscipline, ch: u8) {
    let effect = discipline.handle_byte(ch);

    match effect.echo {
        EchoAction::None => {}
        EchoAction::Bytes(bytes) => ctx.forward_to_console(bytes),
        EchoAction::Byte(byte) => ctx.forward_to_console(&[byte]),
    }

    if let Some(line) = effect.line_ready {
        deliver_line(ctx, &line);
    }
}

/// Deliver a completed line to the shell if subscribed.
fn deliver_line(ctx: &mut TtyContext, line: &[u8]) {
    if ctx.shell_stdin == 0 {
        return;
    }
    let _ = libcluu::ipc::send_with_payload(ctx.shell_stdin, TTY_READ_LABEL, line);
}
