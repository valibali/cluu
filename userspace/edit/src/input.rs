//! Raw-mode TTY input → `KeyEvent` decoder.
//!
//! Esc-vs-escape-sequence disambiguation uses a 25ms read timeout.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEvent {
    Char(char),
    Ctrl(char),
    Esc,
    Arrow(Direction),
    PageUp,
    PageDown,
    Home,
    End,
    Delete,
    Backspace,
    Enter,
    Tab,
    ShiftTab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction { Up, Down, Left, Right }

/// Trait abstracts over the reader so the decoder is testable without TTY.
pub trait ByteReader {
    fn read_byte(&mut self) -> Option<u8>;
    fn read_byte_with_timeout_ms(&mut self, ms: u64) -> Option<u8>;
}

/// Decode the next KeyEvent from a byte stream.
pub fn decode<R: ByteReader>(r: &mut R) -> Option<KeyEvent> {
    let first = r.read_byte()?;
    Some(match first {
        0x1B => decode_escape(r),
        0x09 => KeyEvent::Tab,
        0x0A | 0x0D => KeyEvent::Enter,
        0x7F | 0x08 => KeyEvent::Backspace,
        b if b < 0x20 => KeyEvent::Ctrl((b + b'a' - 1) as char),
        b => {
            // Naive 1-byte char path. UTF-8 multibyte entry is rare in
            // the editor's INSERT mode (most users type ASCII); we accept
            // 1-byte at a time and let multi-byte chars come through as
            // multiple `Char` events. If a future `:lang utf-8` mode is
            // wanted, decode UTF-8 here. For v1 this is fine — see spec §5.
            KeyEvent::Char(b as char)
        }
    })
}

fn decode_escape<R: ByteReader>(r: &mut R) -> KeyEvent {
    // 25ms timeout: if no follow-up byte, this is a bare Esc.
    let Some(next) = r.read_byte_with_timeout_ms(25) else {
        return KeyEvent::Esc;
    };
    if next != b'[' {
        // Esc + something else — treat as bare Esc and discard the byte.
        return KeyEvent::Esc;
    }
    // CSI sequence. Read until we see a final byte (0x40..=0x7E).
    let mut buf = [0u8; 8];
    let mut len = 0;
    while len < buf.len() {
        let Some(b) = r.read_byte_with_timeout_ms(25) else { break };
        buf[len] = b;
        len += 1;
        if b >= 0x40 && b <= 0x7E { break; }
    }
    match &buf[..len] {
        [b'A']                      => KeyEvent::Arrow(Direction::Up),
        [b'B']                      => KeyEvent::Arrow(Direction::Down),
        [b'C']                      => KeyEvent::Arrow(Direction::Right),
        [b'D']                      => KeyEvent::Arrow(Direction::Left),
        [b'H']                      => KeyEvent::Home,
        [b'F']                      => KeyEvent::End,
        [b'1', b'~']                => KeyEvent::Home,
        [b'4', b'~']                => KeyEvent::End,
        [b'3', b'~']                => KeyEvent::Delete,
        [b'5', b'~']                => KeyEvent::PageUp,
        [b'6', b'~']                => KeyEvent::PageDown,
        [b'Z']                      => KeyEvent::ShiftTab,
        _                            => KeyEvent::Esc,  // unknown sequence → swallow
    }
}

/// `ByteReader` backed by libcluu's TTY input endpoint (TOKEN_STDIN).
///
/// Reads bytes via TTY_READ_LABEL IPC. Buffers payloads between
/// `read_byte` calls so multi-byte payloads (e.g. CSI sequences from
/// the kbd driver) drain one byte at a time.
pub struct StdinReader {
    stdin_endpoint: usize,
    pending: Vec<u8>, // bytes buffered from a prior recv
}

impl StdinReader {
    pub fn new() -> Self {
        let info = libcluu::boot::process_info();
        let stdin = info.tokens[libcluu::boot::TOKEN_STDIN];
        StdinReader { stdin_endpoint: stdin, pending: Vec::new() }
    }

    /// Receive a TTY_READ payload into self.pending, blocking up to `ms`.
    /// Returns true if any bytes were appended to `self.pending`.
    fn recv_payload(&mut self, ms: u64) -> bool {
        // Mirrors the shell's input loop pattern (userspace/shell/src/main.rs:142-162):
        // wait on the stdin endpoint, parse_message, drop anything that isn't
        // TTY_READ_LABEL, then copy the payload bytes into our pending buffer.
        // The editor only listens on stdin (no registry endpoint), so single-token
        // ipc_recv_any is fine.
        let mut buf = [0u8; 128];
        let tokens = [self.stdin_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, ms) {
            Ok((_index, len)) => {
                let Some((msg, payload)) = libcluu::ipc::parse_message(&buf[..len]) else {
                    return false;
                };
                if msg.tag.label != libcluu::ipc::TTY_READ_LABEL {
                    // Unexpected label — drop defensively.
                    return false;
                }
                if !payload.is_empty() {
                    self.pending.extend_from_slice(payload);
                    true
                } else if msg.tag.words >= 2 {
                    // Inline single-byte form: words[1] holds the char.
                    let ch = msg.words[1] as u8;
                    self.pending.push(ch);
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }
}

impl ByteReader for StdinReader {
    fn read_byte(&mut self) -> Option<u8> {
        if self.pending.is_empty() {
            // Block (long timeout) until input arrives.
            self.recv_payload(60_000);
        }
        if self.pending.is_empty() {
            None
        } else {
            Some(self.pending.remove(0))
        }
    }

    fn read_byte_with_timeout_ms(&mut self, ms: u64) -> Option<u8> {
        if self.pending.is_empty() {
            self.recv_payload(ms);
        }
        if self.pending.is_empty() {
            None
        } else {
            Some(self.pending.remove(0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    struct VecReader { bytes: Vec<u8>, idx: usize }
    impl ByteReader for VecReader {
        fn read_byte(&mut self) -> Option<u8> {
            if self.idx >= self.bytes.len() { return None; }
            let b = self.bytes[self.idx];
            self.idx += 1;
            Some(b)
        }
        fn read_byte_with_timeout_ms(&mut self, _: u64) -> Option<u8> { self.read_byte() }
    }

    fn r(bytes: &[u8]) -> VecReader { VecReader { bytes: bytes.to_vec(), idx: 0 } }

    #[test]
    fn plain_char() {
        let mut rd = r(b"a");
        assert_eq!(decode(&mut rd), Some(KeyEvent::Char('a')));
    }
    #[test]
    fn ctrl_a() {
        let mut rd = r(&[0x01]);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Ctrl('a')));
    }
    #[test]
    fn bare_esc() {
        let mut rd = r(&[0x1B]);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Esc));
    }
    #[test]
    fn arrow_up() {
        let mut rd = r(&[0x1B, b'[', b'A']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Arrow(Direction::Up)));
    }
    #[test]
    fn page_down() {
        let mut rd = r(&[0x1B, b'[', b'6', b'~']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::PageDown));
    }
    #[test]
    fn delete_key() {
        let mut rd = r(&[0x1B, b'[', b'3', b'~']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Delete));
    }
    #[test]
    fn enter_lf() {
        let mut rd = r(&[0x0A]);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Enter));
    }
}
