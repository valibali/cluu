//! SIMD optimizations for framebuffer operations.
//!
//! Uses SSE2 intrinsics (guaranteed on x86_64) to accelerate bulk pixel operations.
//! SSE2 provides 128-bit registers that can hold 4 u32 pixels at once.

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

/// Fill a row of pixels using SSE2 (4 pixels at a time).
///
/// # Safety
/// - `dst` must be valid for writing `len` u32 values
/// - `len` must be > 0
/// - `color` is the u32 color value to fill with
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
pub unsafe fn fill_row_simd(dst: *mut u32, color: u32, len: usize) {
    // Broadcast the color to all 4 lanes of an SSE register
    let color_vec = _mm_set1_epi32(color as i32);

    // Process 4 pixels at a time (16 bytes = 128 bits)
    let simd_count = len / 4;
    let mut i = 0;

    // Check if destination is 16-byte aligned for optimal performance
    let aligned = (dst as usize).is_multiple_of(16);

    if aligned {
        // Aligned writes: use aligned store for better performance
        while i < simd_count {
            _mm_store_si128(dst.add(i * 4) as *mut __m128i, color_vec);
            i += 1;
        }
    } else {
        // Unaligned writes: use unaligned store
        while i < simd_count {
            _mm_storeu_si128(dst.add(i * 4) as *mut __m128i, color_vec);
            i += 1;
        }
    }

    // Handle remaining pixels (tail)
    let tail_start = i * 4;
    for j in tail_start..len {
        dst.add(j).write_volatile(color);
    }
}

/// Copy a row of pixels using SSE2 (4 pixels at a time).
///
/// # Safety
/// - `src` and `dst` must be valid for reading/writing `len` u32 values
/// - `src` and `dst` must not overlap (use copy_nonoverlapping semantics)
/// - `len` must be > 0
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
pub unsafe fn copy_row_simd(src: *const u32, dst: *mut u32, len: usize) {
    // Process 4 pixels at a time (16 bytes = 128 bits)
    let simd_count = len / 4;
    let mut i = 0;

    // Check alignment for optimal performance
    let src_aligned = (src as usize).is_multiple_of(16);
    let dst_aligned = (dst as usize).is_multiple_of(16);

    if src_aligned && dst_aligned {
        // Both aligned: use aligned loads/stores
        while i < simd_count {
            let data = _mm_load_si128(src.add(i * 4) as *const __m128i);
            _mm_store_si128(dst.add(i * 4) as *mut __m128i, data);
            i += 1;
        }
    } else {
        // At least one unaligned: use unaligned loads/stores
        while i < simd_count {
            let data = _mm_loadu_si128(src.add(i * 4) as *const __m128i);
            _mm_storeu_si128(dst.add(i * 4) as *mut __m128i, data);
            i += 1;
        }
    }

    // Handle remaining pixels (tail)
    let tail_start = i * 4;
    for j in tail_start..len {
        dst.add(j).write_volatile(src.add(j).read_volatile());
    }
}

/// Write a row of pixels from a slice using SSE2 (4 pixels at a time).
///
/// # Safety
/// - `dst` must be valid for writing `len` u32 values
/// - `colors` must have at least `len` elements
/// - `len` must be > 0
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
pub unsafe fn write_row_simd(dst: *mut u32, colors: &[u32], len: usize) {
    // Process 4 pixels at a time
    let simd_count = len / 4;
    let mut i = 0;

    // Check alignment
    let dst_aligned = (dst as usize).is_multiple_of(16);
    let src_aligned = (colors.as_ptr() as usize).is_multiple_of(16);

    if src_aligned && dst_aligned {
        // Both aligned: use aligned loads/stores
        while i < simd_count {
            let data = _mm_load_si128(colors.as_ptr().add(i * 4) as *const __m128i);
            _mm_store_si128(dst.add(i * 4) as *mut __m128i, data);
            i += 1;
        }
    } else {
        // At least one unaligned: use unaligned loads/stores
        while i < simd_count {
            let data = _mm_loadu_si128(colors.as_ptr().add(i * 4) as *const __m128i);
            _mm_storeu_si128(dst.add(i * 4) as *mut __m128i, data);
            i += 1;
        }
    }

    // Handle remaining pixels (tail) - use volatile for device memory
    let tail_start = i * 4;
    for j in tail_start..len {
        dst.add(j).write_volatile(colors[j]);
    }
}

/// Check if SSE2 is available at runtime.
/// On x86_64, SSE2 is always available, so this always returns true.
#[cfg(target_arch = "x86_64")]
pub fn is_sse2_available() -> bool {
    true // SSE2 is guaranteed on x86_64
}
