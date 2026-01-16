#![no_std]
#![no_main]

//! Keyboard service for CLUU.
//!
//! This service receives raw IRQ scancodes, tracks modifier state, converts
//! scancodes to ASCII, and forwards key events to the tty via the registry.

mod context;
mod protocol;
mod scancode;

use context::{idle_on_error, KbdContext};
use libcluu::ipc::KBD_EVENT_LABEL;
use libcluu::types::Message;
use libcluu::Result;
use protocol::{build_kbd_event, parse_message};
use scancode::ScancodeDecoder;

#[no_mangle]
/// Kernel entrypoint for the keyboard service.
///
/// Delegates to `run` to keep the ABI surface small.
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

/// Main service loop: subscribe to tty, decode scancodes, emit key events.
fn run() -> Result<()> {
    let mut ctx = KbdContext::new()?;
    let mut decoder = ScancodeDecoder::new();
    let mut buf = [0u8; 64];
    let mut saw_error = false;

    loop {
        ctx.ensure_tty_subscription();

        let tokens = [ctx.endpoint, ctx.registry_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                let Some((msg, payload)) = parse_message(&buf[..len]) else {
                    continue;
                };

                if index == 1 {
                    ctx.handle_registry_message(&msg, payload);
                    continue;
                }

                handle_kbd_message(&mut ctx, &mut decoder, &msg);
            }
            Err(err) => idle_on_error(err, &mut saw_error),
        }
    }
}

/// Decode a keyboard IRQ message into an ASCII event and forward it.
fn handle_kbd_message(ctx: &mut KbdContext, decoder: &mut ScancodeDecoder, msg: &Message) {
    if msg.tag.label != KBD_EVENT_LABEL || msg.tag.words < 1 {
        return;
    }

    let scancode = msg.words[0] as u8;
    if let Some(event) = decoder.handle_scancode(scancode) {
        if let Some(ascii) = event.ascii {
            let outbound = build_kbd_event(Some(ascii), event.scancode, event.modifiers.as_bits());
            ctx.send_to_tty(&outbound);
        }
    }
}
