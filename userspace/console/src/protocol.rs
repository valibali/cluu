//! IPC protocol helpers for the console service.
//!
//! This keeps parsing and message decoding in one place so rendering code
//! remains focused on drawing and cursor management.

use core::mem::size_of;
use libcluu::types::Message;

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
