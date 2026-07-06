//! Input event types shared by /dev/input/* read path and inputd:input fast path.

extern crate alloc;

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Key { ascii: u8, scancode: u8, modifiers: u8, extended: u8 },
    Mouse { dx: i32, dy: i32, buttons: u8 },
}

impl InputEvent {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            Self::Key { ascii, scancode, modifiers, extended } => {
                buf.push(0);
                buf.push(*ascii);
                buf.push(*scancode);
                buf.push(*modifiers);
                buf.push(*extended);
            }
            Self::Mouse { dx, dy, buttons } => {
                buf.push(1);
                buf.extend_from_slice(&dx.to_le_bytes());
                buf.extend_from_slice(&dy.to_le_bytes());
                buf.push(*buttons);
            }
        }
        buf
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.is_empty() {
            return None;
        }
        match buf[0] {
            0 if buf.len() >= 5 => Some(Self::Key {
                ascii: buf[1],
                scancode: buf[2],
                modifiers: buf[3],
                extended: buf[4],
            }),
            1 if buf.len() >= 10 => Some(Self::Mouse {
                dx: i32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]),
                dy: i32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]),
                buttons: buf[9],
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrip() {
        let e = InputEvent::Key { ascii: b'a', scancode: 0x1e, modifiers: 0, extended: 0 };
        let buf = e.encode();
        let d = InputEvent::decode(&buf).unwrap();
        assert_eq!(e, d);
    }

    #[test]
    fn mouse_roundtrip() {
        let e = InputEvent::Mouse { dx: -5, dy: 10, buttons: 0x03 };
        let buf = e.encode();
        let d = InputEvent::decode(&buf).unwrap();
        assert_eq!(e, d);
    }

    #[test]
    fn decode_empty_returns_none() {
        assert!(InputEvent::decode(&[]).is_none());
    }
}
