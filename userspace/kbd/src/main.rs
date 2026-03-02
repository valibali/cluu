#![no_std]
#![no_main]

//! Keyboard service for CLUU.
//!
//! This service receives raw IRQ scancodes, tracks modifier state, converts
//! scancodes to ASCII, and forwards key events to the active VT's tty via
//! the registry.  VT switching is intercepted here: Ctrl+Alt+F1..F4 triggers
//! a switch without forwarding the key event.

extern crate alloc;

mod context;
mod layout;
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
    let mut buf = [0u8; 128];
    let mut saw_error = false;

    loop {
        ctx.ensure_subscriptions();

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
///
/// VT switch combos (Ctrl+Alt+F1..F4) are intercepted and consumed —
/// the key event is *not* forwarded to the tty.
fn handle_kbd_message(ctx: &mut KbdContext, decoder: &mut ScancodeDecoder, msg: &Message) {
    if msg.tag.label != KBD_EVENT_LABEL || msg.tag.words < 1 {
        return;
    }

    let scancode = msg.words[0] as u8;

    // Check for VT switch *before* updating decoder state so current
    // modifier state (ctrl+alt already held) is visible.
    if let Some(target_vt) = decoder.detect_vt_switch(scancode) {
        ctx.switch_vt(target_vt as usize);
        // Consume the scancode so it doesn't produce a key event.
        let _ = decoder.handle_scancode(scancode);
        return;
    }

    // Check for Ctrl+Alt+Delete shutdown combo.
    if decoder.detect_shutdown_combo(scancode) {
        ctx.send_shutdown();
        let _ = decoder.handle_scancode(scancode);
        return;
    }

    if let Some(event) = decoder.handle_scancode(scancode) {
        // Forward if there's an ASCII char OR an extended key code (arrows etc.)
        if event.ascii.is_some() || event.extended != 0 {
            let outbound = build_kbd_event(
                event.ascii,
                event.scancode,
                event.modifiers.as_bits(),
                event.extended,
            );
            ctx.send_to_tty(&outbound);
        }
    }
}
