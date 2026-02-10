//! IPC protocol helpers for the keyboard service.
//!
//! This module keeps message encoding/decoding in one place so the rest of
//! the service can focus on scancode processing and registry wiring.

use core::mem::size_of;
use libcluu::ipc::KBD_EVENT_LABEL;
use libcluu::types::Message;

/// Modifier bit flags shipped alongside keyboard events.
pub const MOD_SHIFT: u8 = 1 << 0;
pub const MOD_CTRL: u8 = 1 << 1;
pub const MOD_ALT: u8 = 1 << 2;
pub const MOD_CAPS: u8 = 1 << 3;
pub const MOD_NUM: u8 = 1 << 4;
pub const MOD_SCROLL: u8 = 1 << 5;

/// Extended key codes for non-ASCII keys (arrows, nav keys).
pub const KEY_UP: u8 = 1;
pub const KEY_DOWN: u8 = 2;
pub const KEY_LEFT: u8 = 3;
pub const KEY_RIGHT: u8 = 4;
pub const KEY_HOME: u8 = 5;
pub const KEY_END: u8 = 6;
pub const KEY_DELETE: u8 = 7;

/// Parse an IPC message buffer into a Message header + payload slice.
///
/// This clamps malformed payload sizes to avoid out-of-bounds access.
pub fn parse_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    if buf.len() < size_of::<Message>() {
        return None;
    }
    let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
    let mut payload_len = msg.words[0];
    let header = size_of::<Message>();
    if header + payload_len > buf.len() {
        payload_len = 0;
    }
    let end = header + payload_len;
    Some((msg, &buf[header..end]))
}

/// Build a keyboard event message from ASCII/scancode/modifier/extended state.
///
/// Word layout:
/// - words[1]: ASCII (0 if none)
/// - words[2]: modifier bitmask
/// - words[3]: raw scancode (press/release stripped)
/// - words[4]: extended key code (0 for normal keys)
pub fn build_kbd_event(ascii: Option<u8>, scancode: u8, modifiers: u8, extended: u8) -> Message {
    let ascii_word = ascii.unwrap_or(0) as usize;
    Message::new(
        KBD_EVENT_LABEL,
        [
            0,
            ascii_word,
            modifiers as usize,
            scancode as usize,
            extended as usize,
            0,
        ],
        5,
    )
}
