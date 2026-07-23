//! Compositor INPUT_FORWARD → keymap → LineDiscipline → pts.ready_bytes / echo.
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
use libcluu::input::ForwardedKey;
use libcluu::tty_core::keymap::encode_extended;
use libcluu::tty_core::routing::ServiceAction;
use libcluu::types::Message;

pub fn handle(term: &mut Cluuterm, msg: &Message, _payload: &[u8]) {
    let Some(fk) = ForwardedKey::from_message(&msg.words) else { return; };

    let (ascii, extended) = match fk {
        ForwardedKey::Press { ascii, extended, .. } => (ascii, extended),
        ForwardedKey::Release { .. } => return,
        ForwardedKey::Close => return,
    };

    if let Some(bytes) = encode_extended(extended) {
        let actions = alloc::vec![ServiceAction::DeliverBytes(
            bytes.iter().cloned().collect()
        )];
        term.apply_service_actions(actions);
    } else if ascii != 0 {
        let actions = term.pts.on_input_byte(ascii);
        term.apply_service_actions(actions);
    }
}