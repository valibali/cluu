//! IPC protocol helpers for the TTY service.
//!
//! This keeps parsing and message construction centralized so the service
//! loop stays focused on line discipline and routing.

use libcluu::ipc::KBD_EVENT_LABEL;
use libcluu::types::Message;

pub use libcluu::ipc::parse_message;

/// Parsed key event from the keyboard service.
#[allow(dead_code)]
pub struct KbdEvent {
    pub ascii: u8,
    pub modifiers: u8,
    pub scancode: u8,
    /// Extended key code (arrow keys, home, end, etc.). 0 = normal key.
    pub extended: u8,
}

/// Decode a KBD_EVENT message into a structured key event.
///
/// Accepts events with `ascii == 0` when `extended != 0` (arrow keys etc.).
pub fn decode_kbd_event(msg: &Message) -> Option<KbdEvent> {
    if msg.tag.label != KBD_EVENT_LABEL || msg.tag.words < 2 {
        return None;
    }
    let ascii = msg.words[1] as u8;
    let modifiers = msg.words[2] as u8;
    let scancode = msg.words[3] as u8;
    let extended = msg.words[4] as u8;

    // Accept if there's an ASCII char OR an extended key code
    if ascii == 0 && extended == 0 {
        return None;
    }
    Some(KbdEvent {
        ascii,
        modifiers,
        scancode,
        extended,
    })
}
