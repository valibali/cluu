//! Scancode decoding and modifier tracking.
//!
//! This translates set-1 scancodes into ASCII while tracking modifier state
//! (shift/ctrl/alt/caps/num/scroll). Only key presses generate output.

use crate::layout;
use crate::protocol::{
    MOD_ALT, MOD_CAPS, MOD_CTRL, MOD_NUM, MOD_SCROLL, MOD_SHIFT,
    KEY_UP, KEY_DOWN, KEY_LEFT, KEY_RIGHT, KEY_HOME, KEY_END, KEY_DELETE,
};

/// Snapshot of current modifier state.
#[derive(Copy, Clone, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub altgr: bool,
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
            | (if self.altgr { MOD_ALT } else { 0 })
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
    /// Extended key code for non-ASCII keys (arrows, nav). 0 = normal key.
    pub extended: u8,
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

        let ascii = ascii_for_scancode(code, self.modifiers, was_extended);
        let extended = extended_key_code(code, was_extended);
        Some(KeyEvent {
            ascii,
            scancode: code,
            modifiers: self.modifiers,
            extended,
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
            (0x38, false) => {
                self.modifiers.alt = pressed;
                true
            }
            (0x38, true) => {
                self.modifiers.altgr = pressed;
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
fn ascii_for_scancode(scancode: u8, modifiers: Modifiers, extended: bool) -> Option<u8> {
    match scancode {
        0x1C => Some(b'\n'),
        0x0E => Some(0x08),
        0x0F => Some(b'\t'),
        0x39 => Some(b' '),
        _ => keypad_symbol(scancode, modifiers, extended)
            .or_else(|| layout::translate_scancode(scancode, modifiers)),
    }
}

/// Return keypad digits and operators when NumLock is active.
fn keypad_symbol(scancode: u8, modifiers: Modifiers, extended: bool) -> Option<u8> {
    if extended {
        return match scancode {
            0x35 => Some(b'/'),
            _ => None,
        };
    }
    match scancode {
        0x37 => Some(b'*'),
        0x4A => Some(b'-'),
        0x4E => Some(b'+'),
        0x53 if modifiers.num => Some(b'.'),
        0x47 if modifiers.num => Some(b'7'),
        0x48 if modifiers.num => Some(b'8'),
        0x49 if modifiers.num => Some(b'9'),
        0x4B if modifiers.num => Some(b'4'),
        0x4C if modifiers.num => Some(b'5'),
        0x4D if modifiers.num => Some(b'6'),
        0x4F if modifiers.num => Some(b'1'),
        0x50 if modifiers.num => Some(b'2'),
        0x51 if modifiers.num => Some(b'3'),
        0x52 if modifiers.num => Some(b'0'),
        _ => None,
    }
}

/// Map extended scancodes to extended key codes for navigation keys.
///
/// These are only valid when the 0xE0 prefix was seen (extended = true).
/// When NumLock is off, the same scancodes on the keypad also map here,
/// but those are already handled by `keypad_symbol` when NumLock is on.
fn extended_key_code(scancode: u8, extended: bool) -> u8 {
    if !extended {
        return 0;
    }
    match scancode {
        0x48 => KEY_UP,
        0x50 => KEY_DOWN,
        0x4B => KEY_LEFT,
        0x4D => KEY_RIGHT,
        0x47 => KEY_HOME,
        0x4F => KEY_END,
        0x53 => KEY_DELETE,
        _ => 0,
    }
}
