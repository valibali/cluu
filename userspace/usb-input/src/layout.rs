//! USB HID boot-protocol keyboard translation.
//!
//! HID usage codes (page 0x07) differ from PS/2 set-1 scancodes.
//! We translate HID usage → PS/2 scancode so the compositor's hotkey
//! matcher (which uses PS/2 scancodes) works with USB keyboards.
//! ASCII translation then uses the PS/2 scancode via the layout tables
//! (US or Hungarian QWERTZ), mirroring userspace/kbd/src/layout.rs.

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

/// HID usage codes for F1-F5.
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

/// Translate HID usage code → PS/2 set-1 scancode (make code, bit 7 clear).
/// Returns None for keys we don't map (F-keys beyond F5, media keys, etc.).
pub fn hid_to_ps2_scancode(usage: u8) -> Option<u8> {
    match usage {
        0x04 => Some(0x1E), // a
        0x05 => Some(0x30), // b
        0x06 => Some(0x2E), // c
        0x07 => Some(0x20), // d
        0x08 => Some(0x12), // e
        0x09 => Some(0x21), // f
        0x0A => Some(0x22), // g
        0x0B => Some(0x23), // h
        0x0C => Some(0x17), // i
        0x0D => Some(0x24), // j
        0x0E => Some(0x25), // k
        0x0F => Some(0x26), // l
        0x10 => Some(0x32), // m
        0x11 => Some(0x31), // n
        0x12 => Some(0x18), // o
        0x13 => Some(0x19), // p
        0x14 => Some(0x10), // q
        0x15 => Some(0x13), // r
        0x16 => Some(0x1F), // s
        0x17 => Some(0x14), // t
        0x18 => Some(0x16), // u
        0x19 => Some(0x2F), // v
        0x1A => Some(0x11), // w
        0x1B => Some(0x2D), // x
        0x1C => Some(0x15), // y
        0x1D => Some(0x2C), // z
        0x1E => Some(0x02), // 1
        0x1F => Some(0x03), // 2
        0x20 => Some(0x04), // 3
        0x21 => Some(0x05), // 4
        0x22 => Some(0x06), // 5
        0x23 => Some(0x07), // 6
        0x24 => Some(0x08), // 7
        0x25 => Some(0x09), // 8
        0x26 => Some(0x0A), // 9
        0x27 => Some(0x0B), // 0
        0x28 => Some(0x1C), // Enter
        0x29 => Some(0x01), // ESC
        0x2A => Some(0x0E), // Backspace
        0x2B => Some(0x0F), // Tab
        0x2C => Some(0x39), // Space
        0x2D => Some(0x0C), // -
        0x2E => Some(0x0D), // =
        0x2F => Some(0x1A), // [
        0x30 => Some(0x1B), // ]
        0x31 => Some(0x2B), // \
        0x33 => Some(0x27), // ;
        0x34 => Some(0x28), // '
        0x35 => Some(0x29), // `
        0x36 => Some(0x33), // ,
        0x37 => Some(0x34), // .
        0x38 => Some(0x35), // /
        0x3A => Some(0x3B), // F1
        0x3B => Some(0x3C), // F2
        0x3C => Some(0x3D), // F3
        0x3D => Some(0x3E), // F4
        0x3E => Some(0x3F), // F5
        0x4C => Some(0x53), // Delete
        0x4F => Some(0x47), // Home
        0x50 => Some(0x4F), // End
        0x51 => Some(0x51), // PageDown
        0x52 => Some(0x49), // PageUp
        0x4A => Some(0x48), // Up arrow
        0x4B => Some(0x4B), // Left arrow
        0x4D => Some(0x4D), // Right arrow
        0x4E => Some(0x50), // Down arrow
        _ => None,
    }
}

/// Translate a PS/2 scancode to ASCII using the configured layout.
pub fn translate_scancode(scancode: u8, mods: u8) -> Option<u8> {
    let shift = mods & MOD_SHIFT != 0;
    let ctrl = mods & MOD_CTRL != 0;

    if let Some(letter) = letter_for_scancode(scancode) {
        let uppercase = shift ^ caps_lock();
        let mut ascii = if uppercase {
            letter.to_ascii_uppercase()
        } else {
            letter
        };
        if ctrl {
            ascii = ascii.to_ascii_uppercase() & 0x1F;
        }
        return Some(ascii);
    }

    if shift {
        shifted_symbol(scancode)
    } else {
        base_symbol(scancode)
    }
}

/// Full translation: HID usage → (PS/2 scancode, ASCII).
pub fn translate_usage(usage: u8, mods: u8) -> Option<(u8, u8)> {
    let scancode = hid_to_ps2_scancode(usage)?;
    let ascii = translate_scancode(scancode, mods)?;
    Some((scancode, ascii))
}

fn caps_lock() -> bool {
    false
}

// ── Layout tables ──────────────────────────────────────────────────

fn letter_for_scancode(scancode: u8) -> Option<u8> {
    #[cfg(feature = "hu-layout")]
    {
        hu_letter(scancode).or_else(|| us_letter(scancode))
    }
    #[cfg(not(feature = "hu-layout"))]
    {
        us_letter(scancode)
    }
}

fn base_symbol(scancode: u8) -> Option<u8> {
    #[cfg(feature = "hu-layout")]
    {
        hu_base_symbol(scancode).or_else(|| us_base_symbol(scancode))
    }
    #[cfg(not(feature = "hu-layout"))]
    {
        us_base_symbol(scancode)
    }
}

fn shifted_symbol(scancode: u8) -> Option<u8> {
    #[cfg(feature = "hu-layout")]
    {
        hu_shifted_symbol(scancode).or_else(|| us_shifted_symbol(scancode))
    }
    #[cfg(not(feature = "hu-layout"))]
    {
        us_shifted_symbol(scancode)
    }
}

// ── US QWERTY ──────────────────────────────────────────────────────

fn us_letter(scancode: u8) -> Option<u8> {
    match scancode {
        0x10 => Some(b'q'), 0x11 => Some(b'w'), 0x12 => Some(b'e'),
        0x13 => Some(b'r'), 0x14 => Some(b't'), 0x15 => Some(b'y'),
        0x16 => Some(b'u'), 0x17 => Some(b'i'), 0x18 => Some(b'o'),
        0x19 => Some(b'p'), 0x1E => Some(b'a'), 0x1F => Some(b's'),
        0x20 => Some(b'd'), 0x21 => Some(b'f'), 0x22 => Some(b'g'),
        0x23 => Some(b'h'), 0x24 => Some(b'j'), 0x25 => Some(b'k'),
        0x26 => Some(b'l'), 0x2C => Some(b'z'), 0x2D => Some(b'x'),
        0x2E => Some(b'c'), 0x2F => Some(b'v'), 0x30 => Some(b'b'),
        0x31 => Some(b'n'), 0x32 => Some(b'm'),
        _ => None,
    }
}

fn us_base_symbol(scancode: u8) -> Option<u8> {
    match scancode {
        0x02 => Some(b'1'), 0x03 => Some(b'2'), 0x04 => Some(b'3'),
        0x05 => Some(b'4'), 0x06 => Some(b'5'), 0x07 => Some(b'6'),
        0x08 => Some(b'7'), 0x09 => Some(b'8'), 0x0A => Some(b'9'),
        0x0B => Some(b'0'), 0x0C => Some(b'-'), 0x0D => Some(b'='),
        0x1A => Some(b'['), 0x1B => Some(b']'), 0x27 => Some(b';'),
        0x28 => Some(b'\''), 0x29 => Some(b'`'), 0x2B => Some(b'\\'),
        0x33 => Some(b','), 0x34 => Some(b'.'), 0x35 => Some(b'/'),
        0x1C => Some(b'\n'), 0x39 => Some(b' '), 0x0F => Some(b'\t'),
        0x0E => Some(0x08), 0x01 => Some(0x1B),
        _ => None,
    }
}

fn us_shifted_symbol(scancode: u8) -> Option<u8> {
    match scancode {
        0x02 => Some(b'!'), 0x03 => Some(b'@'), 0x04 => Some(b'#'),
        0x05 => Some(b'$'), 0x06 => Some(b'%'), 0x07 => Some(b'^'),
        0x08 => Some(b'&'), 0x09 => Some(b'*'), 0x0A => Some(b'('),
        0x0B => Some(b')'), 0x0C => Some(b'_'), 0x0D => Some(b'+'),
        0x1A => Some(b'{'), 0x1B => Some(b'}'), 0x27 => Some(b':'),
        0x28 => Some(b'"'), 0x29 => Some(b'~'), 0x2B => Some(b'|'),
        0x33 => Some(b'<'), 0x34 => Some(b'>'), 0x35 => Some(b'?'),
        _ => None,
    }
}

// ── Hungarian QWERTZ ───────────────────────────────────────────────
//
// On a real HU keyboard the top row is 0 ' + ! " # ... and the
// y/z keys are swapped relative to US QWERTY.  We approximate
// accented letters with closest ASCII and map dead keys to simple
// ASCII stand-ins (matching userspace/kbd/src/layout.rs HuLayout).

#[cfg(feature = "hu-layout")]
fn hu_letter(scancode: u8) -> Option<u8> {
    match scancode {
        0x15 => Some(b'z'),  // y position → z in QWERTZ
        0x2C => Some(b'y'),  // z position → y in QWERTZ
        _ => None,
    }
}

#[cfg(feature = "hu-layout")]
fn hu_base_symbol(scancode: u8) -> Option<u8> {
    match scancode {
        0x29 => Some(b'0'),  // 0 is on the backtick key
        0x02 => Some(b'1'),
        0x03 => Some(b'2'),
        0x04 => Some(b'3'),
        0x05 => Some(b'4'),
        0x06 => Some(b'5'),
        0x07 => Some(b'6'),
        0x08 => Some(b'7'),
        0x09 => Some(b'8'),
        0x0A => Some(b'9'),
        0x33 => Some(b','),
        0x34 => Some(b'.'),
        0x35 => Some(b'-'),
        _ => None,
    }
}

#[cfg(feature = "hu-layout")]
fn hu_shifted_symbol(scancode: u8) -> Option<u8> {
    match scancode {
        0x29 => Some(b'?'),
        0x02 => Some(b'\''),
        0x03 => Some(b'"'),
        0x04 => Some(b'+'),
        0x05 => Some(b'!'),
        0x06 => Some(b'%'),
        0x07 => Some(b'/'),
        0x08 => Some(b'='),
        0x09 => Some(b'('),
        0x0A => Some(b')'),
        0x33 => Some(b'?'),
        0x34 => Some(b':'),
        0x35 => Some(b'_'),
        _ => None,
    }
}
