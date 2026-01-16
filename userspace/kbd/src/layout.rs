//! Keyboard layout selection and key maps.
//!
//! Layouts return ASCII-only mappings. Hungarian emits accented letters and
//! dead keys in the real layout, so we approximate with closest ASCII
//! characters and map dead keys to simple ASCII stand-ins. US is the fallback
//! for any unmapped scancodes.

use crate::scancode::Modifiers;

/// Layout trait for translating scancodes into ASCII.
pub trait KeyLayout {
    fn letter_for_scancode(scancode: u8) -> Option<u8>;
    fn base_symbol(scancode: u8) -> Option<u8>;
    fn shifted_symbol(scancode: u8) -> Option<u8>;
    fn altgr_symbol(scancode: u8) -> Option<u8>;
}

/// US QWERTY layout (baseline).
pub struct UsLayout;

impl KeyLayout for UsLayout {
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

    fn altgr_symbol(_scancode: u8) -> Option<u8> {
        None
    }
}

/// Hungarian standard layout (ASCII-approximate).
pub struct HuLayout;

impl KeyLayout for HuLayout {
    fn letter_for_scancode(scancode: u8) -> Option<u8> {
        match scancode {
            0x15 => Some(b'z'),
            0x2C => Some(b'y'),
            0x0B => Some(b'o'),
            0x0C => Some(b'u'),
            0x0D => Some(b'o'),
            0x1A => Some(b'o'),
            0x1B => Some(b'u'),
            0x27 => Some(b'e'),
            0x28 => Some(b'a'),
            0x2B => Some(b'u'),
            0x56 => Some(b'i'),
            _ => UsLayout::letter_for_scancode(scancode),
        }
    }

    fn base_symbol(scancode: u8) -> Option<u8> {
        match scancode {
            0x29 => Some(b'0'),
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
            _ => UsLayout::base_symbol(scancode),
        }
    }

    fn shifted_symbol(scancode: u8) -> Option<u8> {
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
            _ => UsLayout::shifted_symbol(scancode),
        }
    }

    fn altgr_symbol(scancode: u8) -> Option<u8> {
        // Dead keys and non-ASCII symbols are approximated with ASCII.
        match scancode {
            0x02 => Some(b'~'),
            0x03 => Some(b'^'),
            0x04 => Some(b'^'),
            0x05 => Some(b'~'),
            0x06 => Some(b'0'),
            0x07 => Some(b','),
            0x08 => Some(b'`'),
            0x09 => Some(b'.'),
            0x0A => Some(b'\''),
            0x0B => Some(b'"'),
            0x0C => Some(b'"'),
            0x0D => Some(b','),
            0x10 => Some(b'\\'),
            0x11 => Some(b'|'),
            0x12 => Some(b'A'),
            0x16 => Some(b'E'),
            0x17 => Some(b'I'),
            0x1A => Some(b'/'),
            0x1B => Some(b'*'),
            0x1E => Some(b'a'),
            0x1F => Some(b'd'),
            0x20 => Some(b'D'),
            0x21 => Some(b'['),
            0x22 => Some(b']'),
            0x24 => Some(b'i'),
            0x25 => Some(b'l'),
            0x26 => Some(b'L'),
            0x27 => Some(b'$'),
            0x28 => Some(b's'),
            0x2B => Some(b'$'),
            0x56 => Some(b'<'),
            0x32 => Some(b'<'),
            0x33 => Some(b';'),
            0x34 => Some(b'>'),
            0x35 => Some(b'*'),
            0x2C => Some(b'>'),
            0x2D => Some(b'#'),
            0x2E => Some(b'&'),
            0x2F => Some(b'@'),
            0x30 => Some(b'{'),
            0x31 => Some(b'}'),
            _ => None,
        }
    }
}

/// Translate a scancode using the selected layout, with US fallback.
pub fn translate_scancode(scancode: u8, modifiers: Modifiers) -> Option<u8> {
    if modifiers.altgr {
        if let Some(symbol) = altgr_symbol(scancode) {
            return Some(symbol);
        }
    }
    let letter = letter_for_scancode(scancode);
    if let Some(letter) = letter {
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
    if modifiers.shift {
        shifted_symbol(scancode)
    } else {
        base_symbol(scancode)
    }
}

fn letter_for_scancode(scancode: u8) -> Option<u8> {
    #[cfg(feature = "hu-layout")]
    {
        HuLayout::letter_for_scancode(scancode).or_else(|| UsLayout::letter_for_scancode(scancode))
    }
    #[cfg(not(feature = "hu-layout"))]
    {
        UsLayout::letter_for_scancode(scancode)
    }
}

fn base_symbol(scancode: u8) -> Option<u8> {
    #[cfg(feature = "hu-layout")]
    {
        HuLayout::base_symbol(scancode).or_else(|| UsLayout::base_symbol(scancode))
    }
    #[cfg(not(feature = "hu-layout"))]
    {
        UsLayout::base_symbol(scancode)
    }
}

fn shifted_symbol(scancode: u8) -> Option<u8> {
    #[cfg(feature = "hu-layout")]
    {
        HuLayout::shifted_symbol(scancode).or_else(|| UsLayout::shifted_symbol(scancode))
    }
    #[cfg(not(feature = "hu-layout"))]
    {
        UsLayout::shifted_symbol(scancode)
    }
}

fn altgr_symbol(scancode: u8) -> Option<u8> {
    #[cfg(feature = "hu-layout")]
    {
        HuLayout::altgr_symbol(scancode).or_else(|| UsLayout::altgr_symbol(scancode))
    }
    #[cfg(not(feature = "hu-layout"))]
    {
        UsLayout::altgr_symbol(scancode)
    }
}
