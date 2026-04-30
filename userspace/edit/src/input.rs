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

/// `ByteReader` backed by the TTY service (TOKEN_STDOUT = TTY main endpoint).
///
/// In raw mode the line discipline does not push `TTY_READ_LABEL` messages —
/// it only fills the TTY's `input_queue` and replies to pending
/// `TTY_READ_REQUEST_LABEL` calls (see userspace/tty/src/context.rs:269-302).
/// So the editor must request bytes synchronously via `ipc_call`, the same
/// way `libcluu::posix::file::read_tty` does for `read(0, ...)` — not via
/// `ipc_recv_any` on TOKEN_STDIN (which is a procmgr bridge endpoint that
/// only carries push-mode TTY_READ_LABEL traffic for canonical-mode shells).
pub struct StdinReader {
    tty_endpoint: usize,
    pending: Vec<u8>, // bytes buffered from a prior request
}

impl StdinReader {
    pub fn new() -> Self {
        let info = libcluu::boot::process_info();
        let tty = info.tokens[libcluu::boot::TOKEN_STDOUT];
        StdinReader { tty_endpoint: tty, pending: Vec::new() }
    }

    /// Issue TTY_READ_REQUEST_LABEL and append reply payload to `pending`.
    /// Blocks up to `ms` ms (0 = forever). Returns true if any bytes arrived.
    fn request_bytes(&mut self, ms: u64) -> bool {
        if self.tty_endpoint == 0 {
            let _ = libcluu::debug_print("edit: request_bytes tty_endpoint=0");
            return false;
        }
        // Match libcluu::posix::file::read_tty: request label, words[0]=max_bytes.
        let mut req = libcluu::types::Message::new(
            libcluu::ipc::TTY_READ_REQUEST_LABEL,
            [0; 6],
            1,
        );
        req.words[0] = 128;
        let req_bytes = req.as_bytes();

        let mut reply_buf = [0u8; 256];
        let result = libcluu::syscall::ipc_call_timeout(
            self.tty_endpoint,
            req_bytes,
            &mut reply_buf,
            ms as usize,
        );
        let bytes = match result {
            // Kernel sometimes returns Ok(0) on a timed-out call instead of
            // Err(Timeout) — race in sys_call between the timeout-wake flag
            // and the call-reply slot lookup (see handlers.rs:560-572).
            // Treat zero-byte success as a timeout: no data, retry.
            Ok(0) => return false,
            Ok(b) => b,
            Err(_) => return false,
        };
        let header_size = core::mem::size_of::<libcluu::types::Message>();
        let Some((_msg, payload)) = libcluu::ipc::parse_message(&reply_buf[..bytes]) else {
            return false;
        };
        let real_payload: &[u8] = if !payload.is_empty() {
            payload
        } else if bytes > header_size {
            // Defensive: TTY's reply_with_payload should set words[0] to the
            // payload length, but a stale-reply race can deliver the bytes
            // with words[0]==0. Salvage the trailing bytes — they're our key
            // bytes — rather than dropping them and faking EOF.
            &reply_buf[header_size..bytes]
        } else {
            return false;
        };
        self.pending.extend_from_slice(real_payload);
        true
    }
}

impl ByteReader for StdinReader {
    fn read_byte(&mut self) -> Option<u8> {
        if self.pending.is_empty() {
            // Block (long timeout) until input arrives.
            self.request_bytes(60_000);
        }
        if self.pending.is_empty() {
            None
        } else {
            Some(self.pending.remove(0))
        }
    }

    fn read_byte_with_timeout_ms(&mut self, ms: u64) -> Option<u8> {
        if self.pending.is_empty() {
            self.request_bytes(ms);
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
