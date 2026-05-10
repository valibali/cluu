//! Tier-3 (2x2-cell) rounded corner bitmaps.
//!
//! Each window corner spans a 2x2 grid of cells. Each cell is an 8x16
//! bitmap. 16 unique sub-cell glyphs live here, slotted into CP437
//! indices 0xF0..=0xFF (private-use range, mapped from Unicode
//! U+E000..=U+E00F by `unicode_to_cp437`).
//!
//! Sub-cell layout per corner:
//!   ┌──────────┬──────────┐
//!   │   _NW    │   _NE    │
//!   ├──────────┼──────────┤
//!   │   _SW    │   _SE    │
//!   └──────────┴──────────┘
//!
//! Bitmap encoding: one byte per row, MSB = leftmost pixel.
//!
//! These are first-pass drawings. Once the compositor lands and renders
//! them on screen, tweak the bytes here for visual quality. Treat the
//! exact pixel patterns as approximate.

pub const TIER3_CORNERS: [[u8; 16]; 16] = [
    // 0xE000 TL_NW: top-left corner, top-left sub-cell.
    // Outer curve from cell's right edge upward to cell's bottom.
    [
        0b00000000, 0b00000000, 0b00000000, 0b00000001,
        0b00000011, 0b00000111, 0b00001110, 0b00011100,
        0b00111000, 0b01110000, 0b01100000, 0b11100000,
        0b11000000, 0b11000000, 0b10000000, 0b10000000,
    ],
    // 0xE001 TL_NE: top-left corner, top-right sub-cell.
    // Curve continues from upper-left of this cell down to mid-bottom.
    [
        0b00000000, 0b00000000, 0b00000000, 0b11000000,
        0b11100000, 0b01110000, 0b00111000, 0b00011100,
        0b00001110, 0b00000111, 0b00000011, 0b00000001,
        0b00000001, 0b00000000, 0b00000000, 0b00000000,
    ],
    // 0xE002 TL_SW: top-left corner, bottom-left sub-cell.
    // Vertical edge from top to bottom on the right side.
    [
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
    ],
    // 0xE003 TL_SE: top-left corner, bottom-right sub-cell.
    // Empty interior — content area starts here.
    [0; 16],
    // 0xE004 TR_NW: top-right corner, top-left sub-cell. Empty interior.
    [0; 16],
    // 0xE005 TR_NE: top-right corner, top-right sub-cell.
    // Mirror of TL_NW horizontally.
    [
        0b00000000, 0b00000000, 0b00000000, 0b10000000,
        0b11000000, 0b11100000, 0b01110000, 0b00111000,
        0b00011100, 0b00001110, 0b00000110, 0b00000111,
        0b00000011, 0b00000011, 0b00000001, 0b00000001,
    ],
    // 0xE006 TR_SW: top-right corner, bottom-left sub-cell. Empty.
    [0; 16],
    // 0xE007 TR_SE: top-right corner, bottom-right sub-cell.
    // Vertical edge on the left side of the cell.
    [
        0b00000001, 0b00000001, 0b00000001, 0b00000001,
        0b00000001, 0b00000001, 0b00000001, 0b00000001,
        0b00000001, 0b00000001, 0b00000001, 0b00000001,
        0b00000001, 0b00000001, 0b00000001, 0b00000001,
    ],
    // 0xE008 BL_NW: bottom-left corner, top-left sub-cell.
    // Vertical edge.
    [
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
    ],
    // 0xE009 BL_NE: bottom-left corner, top-right sub-cell. Empty.
    [0; 16],
    // 0xE00A BL_SW: bottom-left corner, bottom-left sub-cell.
    // Vertical mirror of TL_NW.
    [
        0b10000000, 0b10000000, 0b11000000, 0b11100000,
        0b01100000, 0b01110000, 0b00111000, 0b00011100,
        0b00001110, 0b00000111, 0b00000011, 0b00000001,
        0b00000000, 0b00000000, 0b00000000, 0b00000000,
    ],
    // 0xE00B BL_SE: bottom-left corner, bottom-right sub-cell.
    // Vertical mirror of TL_NE.
    [
        0b00000001, 0b00000001, 0b00000011, 0b00000111,
        0b00001110, 0b00011100, 0b00111000, 0b01110000,
        0b11100000, 0b11000000, 0b10000000, 0b00000000,
        0b00000000, 0b00000000, 0b00000000, 0b00000000,
    ],
    // 0xE00C BR_NW: bottom-right corner, top-left sub-cell. Vertical edge.
    [
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
        0b10000000, 0b10000000, 0b10000000, 0b10000000,
    ],
    // 0xE00D BR_NE: bottom-right corner, top-right sub-cell. Vertical edge.
    [
        0b00000001, 0b00000001, 0b00000001, 0b00000001,
        0b00000001, 0b00000001, 0b00000001, 0b00000001,
        0b00000001, 0b00000001, 0b00000001, 0b00000001,
        0b00000001, 0b00000001, 0b00000001, 0b00000001,
    ],
    // 0xE00E BR_SW: bottom-right corner, bottom-left sub-cell.
    // Mirror of BL_SW vertically (i.e. same as BL_SW left/right swap;
    // here it's the rotated version).
    [
        0b10000000, 0b10000000, 0b11000000, 0b11100000,
        0b01100000, 0b01110000, 0b00111000, 0b00011100,
        0b00001110, 0b00000111, 0b00000011, 0b00000001,
        0b00000000, 0b00000000, 0b00000000, 0b00000000,
    ],
    // 0xE00F BR_SE: bottom-right corner, bottom-right sub-cell.
    // Mirror of BL_SE horizontally.
    [
        0b00000001, 0b00000001, 0b00000011, 0b00000111,
        0b00001110, 0b00011100, 0b00111000, 0b01110000,
        0b11100000, 0b11000000, 0b10000000, 0b00000000,
        0b00000000, 0b00000000, 0b00000000, 0b00000000,
    ],
];

/// Map a tier-3 corner Unicode private-use codepoint to its CP437 slot.
///
/// Returns Some(CP437 index in 0xF0..=0xFF) for U+E000..=U+E00F, else None.
#[inline]
pub fn pua_corner_to_cp437(cp: u32) -> Option<u8> {
    if (0xE000..=0xE00F).contains(&cp) {
        Some(0xF0 + (cp - 0xE000) as u8)
    } else {
        None
    }
}
