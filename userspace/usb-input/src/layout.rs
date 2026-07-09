//! USB HID boot-protocol keyboard usage-code → ASCII translation (US layout).
//!
//! HID usage codes (page 0x07) differ from PS/2 set-1 scancodes.
//! This module maps the subset that matters for terminal input.

const MOD_LCTRL: u8 = 1 << 0;
const MOD_LSHIFT: u8 = 1 << 1;
const MOD_LALT: u8 = 1 << 2;
const MOD_LGUI: u8 = 1 << 3;
const MOD_RCTRL: u8 = 1 << 4;
const MOD_RSHIFT: u8 = 1 << 5;
const MOD_RALT: u8 = 1 << 6;
const MOD_RGUI: u8 = 1 << 7;

pub const MOD_SHIFT: u8 = 1 << 0;
pub const MOD_CTRL: u8 = 1 << 1;
pub const MOD_ALT: u8 = 1 << 2;

pub fn hid_modifiers_to_kbd(hid_mods: u8) -> u8 {
    let mut out = 0u8;
    if hid_mods & (MOD_LSHIFT | MOD_RSHIFT) != 0 {
        out |= MOD_SHIFT;
    }
    if hid_mods & (MOD_LCTRL | MOD_RCTRL) != 0 {
        out |= MOD_CTRL;
    }
    if hid_mods & (MOD_LALT | MOD_RALT) != 0 {
        out |= MOD_ALT;
    }
    out
}

pub fn is_ctrl_alt(mods: u8) -> bool {
    mods & (MOD_CTRL | MOD_ALT) == (MOD_CTRL | MOD_ALT)
}

pub fn vt_switch_target(usage: u8) -> Option<usize> {
    match usage {
        0x3A => Some(0),
        0x3B => Some(1),
        0x3C => Some(2),
        0x3D => Some(3),
        0x3E => Some(4),
        _ => None,
    }
}

pub const HID_USAGE_DELETE: u8 = 0x4C;

pub fn translate_usage(usage: u8, mods: u8) -> Option<u8> {
    let shift = mods & MOD_SHIFT != 0;
    let ctrl = mods & MOD_CTRL != 0;
    match usage {
        0x04..=0x1D => {
            let letter = b'a' + (usage - 0x04);
            if ctrl {
                Some(letter.to_ascii_uppercase() & 0x1F)
            } else if shift {
                Some(letter.to_ascii_uppercase())
            } else {
                Some(letter)
            }
        }
        0x1E..=0x27 => {
            let n = usage - 0x1E;
            let base = match n {
                0 => b'1', 1 => b'2', 2 => b'3', 3 => b'4', 4 => b'5',
                5 => b'6', 6 => b'7', 7 => b'8', 8 => b'9', 9 => b'0',
                _ => return None,
            };
            let shifted = match n {
                0 => b'!', 1 => b'@', 2 => b'#', 3 => b'$', 4 => b'%',
                5 => b'^', 6 => b'&', 7 => b'*', 8 => b'(', 9 => b')',
                _ => return None,
            };
            Some(if shift { shifted } else { base })
        }
        0x28 => Some(b'\n'),
        0x29 => Some(0x1B),
        0x2A => Some(0x08),
        0x2B => Some(b'\t'),
        0x2C => Some(b' '),
        0x2D => Some(if shift { b'_' } else { b'-' }),
        0x2E => Some(if shift { b'+' } else { b'=' }),
        0x2F => Some(if shift { b'{' } else { b'[' }),
        0x30 => Some(if shift { b'}' } else { b']' }),
        0x31 => Some(if shift { b'|' } else { b'\\' }),
        0x33 => Some(if shift { b':' } else { b';' }),
        0x34 => Some(if shift { b'"' } else { b'\'' }),
        0x35 => Some(if shift { b'~' } else { b'`' }),
        0x36 => Some(if shift { b'<' } else { b',' }),
        0x37 => Some(if shift { b'>' } else { b'.' }),
        0x38 => Some(if shift { b'?' } else { b'/' }),
        _ => None,
    }
}
