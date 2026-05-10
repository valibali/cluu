// userspace/console/src/atlas.rs
//! Per-glyph mask atlas. Each entry is GLYPH_W * GLYPH_H u32 words; a set bit
//! in the source 8x16 font becomes 0xFFFF_FFFF, a clear bit becomes 0.
//! Per-cell rendering can then SIMD-blend `(mask & fg) | (!mask & bg)` instead
//! of branching on each bit.

extern crate alloc;

use alloc::boxed::Box;

pub const GLYPH_W: usize = 8;
pub const GLYPH_H: usize = 16;
pub const ATLAS_ENTRIES: usize = 256;
pub const ATLAS_STRIDE: usize = GLYPH_W * GLYPH_H;          // 128 u32 per glyph
pub const ATLAS_LEN: usize = ATLAS_ENTRIES * ATLAS_STRIDE;  // 32 768 u32 = 128 KiB

pub struct GlyphAtlas {
    masks: Box<[u32; ATLAS_LEN]>,
}

impl GlyphAtlas {
    /// Build a fresh atlas by expanding each font row byte into 8 u32 mask
    /// entries. `font_bits[ch * GLYPH_H + row]` provides the bit pattern.
    pub fn from_font(font_bits: &[u8]) -> Self {
        // Heap-allocate so we don't blow the userspace stack.
        let mut masks: Box<[u32; ATLAS_LEN]> = Box::new([0u32; ATLAS_LEN]);
        for ch in 0..ATLAS_ENTRIES {
            for row in 0..GLYPH_H {
                let line = font_bits[ch * GLYPH_H + row];
                let row_off = ch * ATLAS_STRIDE + row * GLYPH_W;
                for col in 0..GLYPH_W {
                    let bit = (line >> (7 - col)) & 1;
                    masks[row_off + col] = if bit != 0 { 0xFFFF_FFFFu32 } else { 0 };
                }
            }
        }
        Self { masks }
    }

    /// Borrow one row of the mask for `ch`.
    #[inline]
    pub fn row(&self, ch: u8, row: usize) -> &[u32; GLYPH_W] {
        let off = (ch as usize) * ATLAS_STRIDE + row * GLYPH_W;
        // SAFETY: GLYPH_W == 8, off + 8 <= ATLAS_LEN by construction.
        unsafe { &*(self.masks[off..off + GLYPH_W].as_ptr() as *const [u32; GLYPH_W]) }
    }
}
