//! SIMD helpers usable by any userspace renderer.
//!
//! Currently exposes `blend_row` (PAND/PANDN/POR per 4-pixel chunk under
//! SSE2, scalar fallback elsewhere) plus its `is_sse2_available` runtime
//! probe. Move other simd helpers here as more crates need them.

#[cfg(target_arch = "x86_64")]
#[inline]
pub fn is_sse2_available() -> bool {
    // SSE2 is mandatory on x86_64 (per ABI), so this is always true on
    // the kernels CLUU targets. Wrap in a fn for potential future
    // tighter detection (AVX2/AVX-512).
    true
}

#[cfg(not(target_arch = "x86_64"))]
#[inline]
pub fn is_sse2_available() -> bool {
    false
}

/// Blend `dst[i] = (mask[i] & fg) | (!mask[i] & bg)` for `i in 0..len`,
/// where `len = min(mask.len(), dst.len())`.
#[inline]
pub fn blend_row(mask: &[u32], fg: u32, bg: u32, dst: &mut [u32]) {
    let len = mask.len().min(dst.len());

    #[cfg(target_arch = "x86_64")]
    {
        if is_sse2_available() && len >= 4 {
            unsafe { blend_row_sse2(mask, fg, bg, dst, len) };
            return;
        }
    }

    for i in 0..len {
        dst[i] = (mask[i] & fg) | (!mask[i] & bg);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn blend_row_sse2(mask: &[u32], fg: u32, bg: u32, dst: &mut [u32], len: usize) {
    use core::arch::x86_64::*;
    let fg_v = _mm_set1_epi32(fg as i32);
    let bg_v = _mm_set1_epi32(bg as i32);
    let chunks = len / 4;
    for chunk in 0..chunks {
        let off = chunk * 4;
        let m = _mm_loadu_si128(mask.as_ptr().add(off) as *const _);
        let lhs = _mm_and_si128(m, fg_v);
        let rhs = _mm_andnot_si128(m, bg_v);
        let out = _mm_or_si128(lhs, rhs);
        _mm_storeu_si128(dst.as_mut_ptr().add(off) as *mut _, out);
    }
    let tail = chunks * 4;
    for i in tail..len {
        dst[i] = (mask[i] & fg) | (!mask[i] & bg);
    }
}

/// Alpha-blend `dst[i] = lerp(bg, fg, alpha[i]/255)` for 8 pixels.
/// Pixel format is 0x00RRGGBB. Uses gamma-correct blending via a
/// compile-time sRGB LUT: fg/bg are converted to linear, blended,
/// then converted back to sRGB. Edges appear sharper than linear-in-sRGB.
#[inline]
pub fn blend_alpha_row(alpha: &[u8], fg: u32, bg: u32, dst: &mut [u32]) {
    let len = alpha.len().min(dst.len());

    let fg_r = SRGB_TO_LINEAR[((fg >> 16) & 0xFF) as usize];
    let fg_g = SRGB_TO_LINEAR[((fg >> 8) & 0xFF) as usize];
    let fg_b = SRGB_TO_LINEAR[(fg & 0xFF) as usize];
    let bg_r = SRGB_TO_LINEAR[((bg >> 16) & 0xFF) as usize];
    let bg_g = SRGB_TO_LINEAR[((bg >> 8) & 0xFF) as usize];
    let bg_b = SRGB_TO_LINEAR[(bg & 0xFF) as usize];

    let mut lut = [0u32; 256];
    lut[0] = bg;
    lut[255] = fg;
    for a in 1..255 {
        let af = a as u32;
        let inv = 255 - af;
        let r_lin = (fg_r * af + bg_r * inv) / 255;
        let g_lin = (fg_g * af + bg_g * inv) / 255;
        let b_lin = (fg_b * af + bg_b * inv) / 255;
        lut[a] =
            (LINEAR_TO_SRGB[r_lin as usize] as u32) << 16
            | (LINEAR_TO_SRGB[g_lin as usize] as u32) << 8
            | (LINEAR_TO_SRGB[b_lin as usize] as u32);
    }

    for i in 0..len {
        dst[i] = lut[alpha[i] as usize];
    }
}

const SRGB_TO_LINEAR: [u32; 256] = include!(concat!(env!("OUT_DIR"), "/srgb_lut.rs"));
const LINEAR_TO_SRGB: [u8; 256] = include!(concat!(env!("OUT_DIR"), "/linear_srgb_lut.rs"));
