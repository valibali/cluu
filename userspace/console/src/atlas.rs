//! 256-entry byte → [u32; 8] mask LUT for SSE2-friendly glyph row blits.
//!
//! For an 8-bit font row, each bit becomes one u32 mask word
//! (`0xFFFF_FFFF` for set, `0` for clear). Expanding once into a 256-entry
//! lookup keeps the working set in L1 (~8 KiB) regardless of how many
//! distinct glyphs are on screen — full-screen redraws no longer thrash
//! through a per-char atlas.

pub const GLYPH_W: usize = 8;
pub const GLYPH_H: usize = 16;

/// Static lookup, computed at compile time. 256 × 8 × 4 = 8 KiB in `.rodata`.
pub static BYTE_MASK_LUT: [[u32; GLYPH_W]; 256] = build_lut();

const fn build_lut() -> [[u32; GLYPH_W]; 256] {
    let mut lut = [[0u32; GLYPH_W]; 256];
    let mut b = 0usize;
    while b < 256 {
        let mut col = 0usize;
        while col < GLYPH_W {
            let bit = (b >> (7 - col)) & 1;
            lut[b][col] = if bit != 0 { 0xFFFF_FFFFu32 } else { 0 };
            col += 1;
        }
        b += 1;
    }
    lut
}

/// Look up the 8-pixel mask row for a font row byte.
#[inline]
pub fn mask_for_byte(byte: u8) -> &'static [u32; GLYPH_W] {
    &BYTE_MASK_LUT[byte as usize]
}
