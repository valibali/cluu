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
    masks: Box<[u32]>,
}

impl GlyphAtlas {
    /// Build a fresh atlas by expanding each font row byte into 8 u32 mask
    /// entries. `font_bits[ch * GLYPH_H + row]` provides the bit pattern.
    pub fn from_font(font_bits: &[u8]) -> Self {
        // Allocate directly on the heap via Vec; no large stack temporary.
        let mut masks: alloc::vec::Vec<u32> = alloc::vec![0u32; ATLAS_LEN];
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
        Self { masks: masks.into_boxed_slice() }
    }

    /// Borrow one row of the mask for `ch`.
    #[inline]
    pub fn row(&self, ch: u8, row: usize) -> &[u32; GLYPH_W] {
        debug_assert!(row < GLYPH_H);
        let off = (ch as usize) * ATLAS_STRIDE + row * GLYPH_W;
        // SAFETY: ch is u8 (< 256 = ATLAS_ENTRIES) and row < GLYPH_H by debug_assert
        // above, so off + GLYPH_W <= ATLAS_LEN. Slice indexing also bounds-checks.
        let slice: &[u32] = &self.masks[off..off + GLYPH_W];
        unsafe { &*(slice.as_ptr() as *const [u32; GLYPH_W]) }
    }
}
