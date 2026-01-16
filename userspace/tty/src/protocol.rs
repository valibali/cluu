//! IPC protocol helpers for the TTY service.
//!
//! This keeps parsing and message construction centralized so the service
//! loop stays focused on line discipline and routing.

use core::mem::size_of;
use libcluu::ipc::KBD_EVENT_LABEL;
use libcluu::types::Message;

/// Parsed key event from the keyboard service.
#[allow(dead_code)]
pub struct KbdEvent {
    pub ascii: u8,
    pub modifiers: u8,
    pub scancode: u8,
}

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

/// Decode a KBD_EVENT message into a structured key event.
pub fn decode_kbd_event(msg: &Message) -> Option<KbdEvent> {
    if msg.tag.label != KBD_EVENT_LABEL || msg.tag.words < 2 {
        return None;
    }
    let ascii = msg.words[1] as u8;
    let modifiers = msg.words[2] as u8;
    let scancode = msg.words[3] as u8;
    if ascii == 0 {
        return None;
    }
    Some(KbdEvent {
        ascii,
        modifiers,
        scancode,
    })
}
