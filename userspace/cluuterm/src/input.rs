//! Compositor INPUT_FORWARD → keymap → LineDiscipline → stdin_buf / echo.
//!
//! # Wire layout (COMP_INPUT_FORWARD_LABEL)
//!
//! | word | field    | notes                                    |
//! |------|----------|------------------------------------------|
//! |  0   | window_id| the focused window's id                  |
//! |  1   | ascii    | printable/control byte (0 if none)       |
//! |  2   | mods     | modifier bitmask (unused here)           |
//! |  3   | scancode | hardware scancode (unused here)          |
//! |  4   | extended | KEY_* enum from kbd driver               |
//! |  5   | kind     | 0 = ordinary input, 99 = close-request   |
//!
//! The close-request path (kind=99) is handled by the recv loop before it
//! reaches this function; we only ever see kind=0 here.

use crate::tty_backend::Cluuterm;
use libcluu::tty_core::keymap::encode_extended;
use libcluu::tty_core::routing::route_input_byte;
use libcluu::types::Message;
use libcluu::debug_print;

/// Handle a `COMP_INPUT_FORWARD_LABEL` message.
///
/// Extracts the key event, runs it through the shared keymap, then feeds
/// each byte through `route_input_byte` which drives the unified PTS verb
/// set line discipline. The resulting `ServiceAction`s are dispatched via
/// `term.apply_service_actions`.
pub fn handle(term: &mut Cluuterm, msg: &Message, _payload: &[u8]) {
    let ascii    = msg.words[1] as u8;
    let extended = msg.words[4] as u8;

    if let Some(bytes) = encode_extended(extended) {
        // Log the CSI sequence (hex) for harness observability.
        // Arrow keys produce 3-byte sequences: ESC [ A/B/C/D.
        let mut logbuf = *b"cluuterm: input csi 00";
        let hex = b"0123456789abcdef";
        // Encode first byte of the sequence (0x1b for CSI).
        if !bytes.is_empty() {
            logbuf[20] = hex[(bytes[0] >> 4) as usize];
            logbuf[21] = hex[(bytes[0] & 0xF) as usize];
        }
        let s_str = unsafe { core::str::from_utf8_unchecked(&logbuf) };
        let _ = debug_print(s_str);
        for &b in bytes {
            let actions = term.pts.on_input_byte(b);
            term.apply_service_actions(actions);
        }
    } else if ascii != 0 {
        let actions = term.pts.on_input_byte(ascii);
        term.apply_service_actions(actions);
    }
}