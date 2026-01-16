//! Scancode decoding and modifier tracking.
//!
//! This translates set-1 scancodes into ASCII while tracking modifier state
//! (shift/ctrl/alt/caps/num/scroll). Only key presses generate output.

use crate::protocol::{MOD_ALT, MOD_CAPS, MOD_CTRL, MOD_NUM, MOD_SCROLL, MOD_SHIFT};

/// Snapshot of current modifier state.
#[derive(Copy, Clone, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub caps: bool,
    pub num: bool,
    pub scroll: bool,
}

impl Modifiers {
    /// Encode modifier state into a bitmask for IPC messages.
    pub fn as_bits(self) -> u8 {
        (if self.shift { MOD_SHIFT } else { 0 })
            | (if self.ctrl { MOD_CTRL } else { 0 })
            | (if self.alt { MOD_ALT } else { 0 })
            | (if self.caps { MOD_CAPS } else { 0 })
            | (if self.num { MOD_NUM } else { 0 })
            | (if self.scroll { MOD_SCROLL } else { 0 })
    }
}

/// Output of a scancode decode (only for key presses).
pub struct KeyEvent {
    pub ascii: Option<u8>,
    pub scancode: u8,
    pub modifiers: Modifiers,
}

/// Stateful decoder for set-1 scancodes.
pub struct ScancodeDecoder {
    extended: bool,
    modifiers: Modifiers,
}

impl ScancodeDecoder {
    /// Create a fresh decoder with no modifiers active.
    pub fn new() -> Self {
        Self {
            extended: false,
            modifiers: Modifiers::default(),
        }
    }

    /// Return the current modifier snapshot.
    #[allow(dead_code)]
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Process a raw scancode, updating modifiers and producing an event.
    ///
    /// Returns `None` for releases or modifier-only presses.
    pub fn handle_scancode(&mut self, scancode: u8) -> Option<KeyEvent> {
        // Extended scancode prefix.
        if scancode == 0xE0 {
            self.extended = true;
            return None;
        }
        if scancode == 0xE1 {
            self.extended = true;
            return None;
        }

        let pressed = scancode & 0x80 == 0;
        let code = scancode & 0x7F;
        let was_extended = self.extended;
        self.extended = false;

        if self.update_modifiers(code, pressed, was_extended) {
            return None;
        }

        if !pressed {
            return None;
        }

        let ascii = ascii_for_scancode(code, self.modifiers);
        Some(KeyEvent {
            ascii,
            scancode: code,
            modifiers: self.modifiers,
        })
    }

    /// Update modifier state. Returns true if the scancode was a modifier.
    fn update_modifiers(&mut self, code: u8, pressed: bool, extended: bool) -> bool {
        match (code, extended) {
            (0x2A, _) | (0x36, _) => {
                self.modifiers.shift = pressed;
                true
            }
            (0x1D, false) | (0x1D, true) => {
                self.modifiers.ctrl = pressed;
                true
            }
            (0x38, false) | (0x38, true) => {
                self.modifiers.alt = pressed;
                true
            }
            (0x3A, _) if pressed => {
                self.modifiers.caps = !self.modifiers.caps;
                true
            }
            (0x45, _) if pressed => {
                self.modifiers.num = !self.modifiers.num;
                true
            }
            (0x46, _) if pressed => {
                self.modifiers.scroll = !self.modifiers.scroll;
                true
            }
            _ => false,
        }
    }
}

/// Convert a scancode into ASCII using the current modifier state.
fn ascii_for_scancode(scancode: u8, modifiers: Modifiers) -> Option<u8> {
    if let Some(letter) = letter_for_scancode(scancode) {
        let uppercase = modifiers.shift ^ modifiers.caps;
        let mut ascii = if uppercase {
            letter.to_ascii_uppercase()
        } else {
            letter
        };
        if modifiers.ctrl {
            ascii = ascii.to_ascii_uppercase() & 0x1F;
        }
        return Some(ascii);
    }

    match scancode {
        0x1C => Some(b'\n'),
        0x0E => Some(0x08),
        0x0F => Some(b'\t'),
        0x39 => Some(b' '),
        _ => None,
    }
    .or_else(|| {
        if modifiers.shift {
            shifted_symbol(scancode)
        } else {
            base_symbol(scancode)
        }
    })
}

/// Return the base ASCII letter for a scancode, or None if not a letter key.
fn letter_for_scancode(scancode: u8) -> Option<u8> {
    match scancode {
        0x10 => Some(b'q'),
        0x11 => Some(b'w'),
        0x12 => Some(b'e'),
        0x13 => Some(b'r'),
        0x14 => Some(b't'),
        0x15 => Some(b'y'),
        0x16 => Some(b'u'),
        0x17 => Some(b'i'),
        0x18 => Some(b'o'),
        0x19 => Some(b'p'),
        0x1E => Some(b'a'),
        0x1F => Some(b's'),
        0x20 => Some(b'd'),
        0x21 => Some(b'f'),
        0x22 => Some(b'g'),
        0x23 => Some(b'h'),
        0x24 => Some(b'j'),
        0x25 => Some(b'k'),
        0x26 => Some(b'l'),
        0x2C => Some(b'z'),
        0x2D => Some(b'x'),
        0x2E => Some(b'c'),
        0x2F => Some(b'v'),
        0x30 => Some(b'b'),
        0x31 => Some(b'n'),
        0x32 => Some(b'm'),
        _ => None,
    }
}

/// Return the unshifted symbol/digit for a scancode.
fn base_symbol(scancode: u8) -> Option<u8> {
    match scancode {
        0x02 => Some(b'1'),
        0x03 => Some(b'2'),
        0x04 => Some(b'3'),
        0x05 => Some(b'4'),
        0x06 => Some(b'5'),
        0x07 => Some(b'6'),
        0x08 => Some(b'7'),
        0x09 => Some(b'8'),
        0x0A => Some(b'9'),
        0x0B => Some(b'0'),
        0x0C => Some(b'-'),
        0x0D => Some(b'='),
        0x1A => Some(b'['),
        0x1B => Some(b']'),
        0x27 => Some(b';'),
        0x28 => Some(b'\''),
        0x29 => Some(b'`'),
        0x2B => Some(b'\\'),
        0x33 => Some(b','),
        0x34 => Some(b'.'),
        0x35 => Some(b'/'),
        _ => None,
    }
}

/// Return the shifted symbol/digit for a scancode.
fn shifted_symbol(scancode: u8) -> Option<u8> {
    match scancode {
        0x02 => Some(b'!'),
        0x03 => Some(b'@'),
        0x04 => Some(b'#'),
        0x05 => Some(b'$'),
        0x06 => Some(b'%'),
        0x07 => Some(b'^'),
        0x08 => Some(b'&'),
        0x09 => Some(b'*'),
        0x0A => Some(b'('),
        0x0B => Some(b')'),
        0x0C => Some(b'_'),
        0x0D => Some(b'+'),
        0x1A => Some(b'{'),
        0x1B => Some(b'}'),
        0x27 => Some(b':'),
        0x28 => Some(b'"'),
        0x29 => Some(b'~'),
        0x2B => Some(b'|'),
        0x33 => Some(b'<'),
        0x34 => Some(b'>'),
        0x35 => Some(b'?'),
        _ => None,
    }
}
