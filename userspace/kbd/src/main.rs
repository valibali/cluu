#![no_std]
#![no_main]

//! Keyboard service for CLUU.
//!
//! This service receives raw IRQ scancodes, tracks modifier state, converts
//! scancodes to ASCII, and forwards all key events to vtmgr:input for
//! routing to the active VT's tty or compositor.  VT switching combos
//! (Ctrl+Alt+F1..F5) are sent to vtmgr:control as switch requests rather
//! than being forwarded as key events.

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

/// Main service loop: subscribe to vtmgr, decode scancodes, emit key events.
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
/// VT switch combos (Ctrl+Alt+F1..F5) are intercepted and sent to
/// vtmgr:control as switch requests — the key event is *not* forwarded.
fn handle_kbd_message(ctx: &mut KbdContext, decoder: &mut ScancodeDecoder, msg: &Message) {
    if msg.tag.label != KBD_EVENT_LABEL || msg.tag.words < 1 {
        return;
    }

    let scancode = msg.words[0] as u8;

    // Check for VT switch *before* updating decoder state so current
    // modifier state (ctrl+alt already held) is visible.
    if let Some(target_vt) = decoder.detect_vt_switch(scancode) {
        ctx.request_vt_switch(target_vt as usize);
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
        // Intercept Shift+PageUp/PageDown for scrollback before forwarding.
        if event.modifiers.shift && event.extended == protocol::KEY_PAGE_UP {
            ctx.send_scroll(0); // scroll back (show older)
            return;
        }
        if event.modifiers.shift && event.extended == protocol::KEY_PAGE_DOWN {
            ctx.send_scroll(1); // scroll forward (show newer)
            return;
        }

        // Forward if there's an ASCII char OR an extended key code (arrows etc.)
        if event.ascii.is_some() || event.extended != 0 {
            let outbound = build_kbd_event(
                event.ascii,
                event.scancode,
                event.modifiers.as_bits(),
                event.extended,
            );
            ctx.send_to_router(&outbound);
        }
    }
}
