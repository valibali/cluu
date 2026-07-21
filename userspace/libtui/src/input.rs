//! Raw-mode TTY input -> `KeyEvent` decoder.
//!
//! Esc-vs-escape-sequence disambiguation uses a 25ms read timeout on the
//! `ByteReader` trait. The pure decode logic is testable without a TTY
//! via the `VecReader` test helper.

extern crate alloc;

#[cfg(feature = "runtime")]
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
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

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
        b => KeyEvent::Char(b as char),
    })
}

fn decode_escape<R: ByteReader>(r: &mut R) -> KeyEvent {
    let Some(next) = r.read_byte_with_timeout_ms(25) else {
        return KeyEvent::Esc;
    };
    if next != b'[' {
        return KeyEvent::Esc;
    }
    let mut buf = [0u8; 8];
    let mut len = 0;
    while len < buf.len() {
        let Some(b) = r.read_byte_with_timeout_ms(25) else {
            break;
        };
        buf[len] = b;
        len += 1;
        if b >= 0x40 && b <= 0x7E {
            break;
        }
    }
    match &buf[..len] {
        [b'A'] => KeyEvent::Arrow(Direction::Up),
        [b'B'] => KeyEvent::Arrow(Direction::Down),
        [b'C'] => KeyEvent::Arrow(Direction::Right),
        [b'D'] => KeyEvent::Arrow(Direction::Left),
        [b'H'] => KeyEvent::Home,
        [b'F'] => KeyEvent::End,
        [b'1', b'~'] => KeyEvent::Home,
        [b'4', b'~'] => KeyEvent::End,
        [b'3', b'~'] => KeyEvent::Delete,
        [b'5', b'~'] => KeyEvent::PageUp,
        [b'6', b'~'] => KeyEvent::PageDown,
        [b'Z'] => KeyEvent::ShiftTab,
        _ => KeyEvent::Esc,
    }
}

/// `ByteReader` backed by stdin (fd 0) via `libcluu::posix::_read`.
///
/// Reads are batched: a single `_read` call fills an internal buffer,
/// and subsequent `read_byte` calls drain it. `read_byte_with_timeout_ms`
/// is non-blocking — if no bytes are buffered, it returns `None`
/// immediately. In practice, TTY drivers deliver escape sequences as a
/// single burst, so bare Esc is detected without a 25ms wait.
#[cfg(feature = "runtime")]
pub struct StdinReader {
    pending: Vec<u8>,
    buf: [u8; 128],
}

#[cfg(feature = "runtime")]
impl StdinReader {
    pub fn new() -> Self {
        StdinReader {
            pending: Vec::new(),
            buf: [0u8; 128],
        }
    }

    pub fn has_data(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn wait_for_data(&mut self, ms: usize) -> bool {
        if !self.pending.is_empty() {
            return true;
        }
        let n = libcluu::posix::file::read_stdin_timeout(&mut self.buf, ms);
        if n > 0 {
            self.pending.extend_from_slice(&self.buf[..n as usize]);
            true
        } else {
            false
        }
    }

    fn fill(&mut self) {
        if !self.pending.is_empty() {
            return;
        }
        let n = libcluu::posix::_read(
            0,
            self.buf.as_mut_ptr() as *mut core::ffi::c_void,
            self.buf.len(),
        );
        if n > 0 {
            self.pending.extend_from_slice(&self.buf[..n as usize]);
        }
    }

    fn fill_timeout(&mut self, ms: usize) {
        if !self.pending.is_empty() {
            return;
        }
        let n = libcluu::posix::file::read_tty_timeout(&mut self.buf, ms);
        if n > 0 {
            self.pending.extend_from_slice(&self.buf[..n as usize]);
        }
    }
}

#[cfg(feature = "runtime")]
impl ByteReader for StdinReader {
    fn read_byte(&mut self) -> Option<u8> {
        self.fill();
        if self.pending.is_empty() {
            None
        } else {
            Some(self.pending.remove(0))
        }
    }

    fn read_byte_with_timeout_ms(&mut self, ms: u64) -> Option<u8> {
        if self.pending.is_empty() {
            self.fill_timeout(ms as usize);
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

    struct VecReader {
        bytes: Vec<u8>,
        idx: usize,
    }
    impl ByteReader for VecReader {
        fn read_byte(&mut self) -> Option<u8> {
            if self.idx >= self.bytes.len() {
                return None;
            }
            let b = self.bytes[self.idx];
            self.idx += 1;
            Some(b)
        }
        fn read_byte_with_timeout_ms(&mut self, _: u64) -> Option<u8> {
            self.read_byte()
        }
    }

    fn r(bytes: &[u8]) -> VecReader {
        VecReader {
            bytes: bytes.to_vec(),
            idx: 0,
        }
    }

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
    fn arrow_down() {
        let mut rd = r(&[0x1B, b'[', b'B']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Arrow(Direction::Down)));
    }

    #[test]
    fn arrow_right() {
        let mut rd = r(&[0x1B, b'[', b'C']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Arrow(Direction::Right)));
    }

    #[test]
    fn arrow_left() {
        let mut rd = r(&[0x1B, b'[', b'D']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Arrow(Direction::Left)));
    }

    #[test]
    fn home_key() {
        let mut rd = r(&[0x1B, b'[', b'H']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Home));
    }

    #[test]
    fn end_key() {
        let mut rd = r(&[0x1B, b'[', b'F']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::End));
    }

    #[test]
    fn page_down() {
        let mut rd = r(&[0x1B, b'[', b'6', b'~']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::PageDown));
    }

    #[test]
    fn page_up() {
        let mut rd = r(&[0x1B, b'[', b'5', b'~']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::PageUp));
    }

    #[test]
    fn delete_key() {
        let mut rd = r(&[0x1B, b'[', b'3', b'~']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Delete));
    }

    #[test]
    fn shift_tab() {
        let mut rd = r(&[0x1B, b'[', b'Z']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::ShiftTab));
    }

    #[test]
    fn enter_lf() {
        let mut rd = r(&[0x0A]);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Enter));
    }

    #[test]
    fn enter_cr() {
        let mut rd = r(&[0x0D]);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Enter));
    }

    #[test]
    fn tab_key() {
        let mut rd = r(&[0x09]);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Tab));
    }

    #[test]
    fn backspace_127() {
        let mut rd = r(&[0x7F]);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Backspace));
    }

    #[test]
    fn backspace_8() {
        let mut rd = r(&[0x08]);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Backspace));
    }

    #[test]
    fn empty_input() {
        let mut rd = r(b"");
        assert_eq!(decode(&mut rd), None);
    }

    #[test]
    fn unknown_csi_becomes_esc() {
        let mut rd = r(&[0x1B, b'[', b'X']);
        assert_eq!(decode(&mut rd), Some(KeyEvent::Esc));
    }
}
