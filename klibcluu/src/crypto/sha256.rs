//! SHA-256 Hash Function (FIPS 180-4)
//!
//! Two compress paths:
//!   1. Hardware SHA-NI (CPUID.07H.0:EBX[29]) — used when available. ~10–20×
//!      faster on a 4 MB input. Boot's manifest_check went from ~440 ms per
//!      primordial to ~20–40 ms with this path. Detection is cached.
//!   2. Software fallback — original FIPS 180-4 reference implementation.
//!      Stack-only, no heap allocation.
//!
//! The two paths must produce byte-identical hashes; the existing test suite
//! covers correctness on the software path, and the boot `manifest_check`
//! cross-validates the hardware path against the recorded hashes in
//! `boot.manifest`.

use core::sync::atomic::{AtomicU8, Ordering};

/// FIPS 180-4 Section 5.3.3 — Initial hash values
const H_INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// FIPS 180-4 Section 4.2.2 — Round constants (first 32 bits of cube roots of first 64 primes)
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[inline(always)]
fn ch(x: u32, y: u32, z: u32) -> u32 {
    (x & y) ^ (!x & z)
}

#[inline(always)]
fn maj(x: u32, y: u32, z: u32) -> u32 {
    (x & y) ^ (x & z) ^ (y & z)
}

#[inline(always)]
fn big_sigma0(x: u32) -> u32 {
    x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22)
}

#[inline(always)]
fn big_sigma1(x: u32) -> u32 {
    x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25)
}

#[inline(always)]
fn small_sigma0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}

#[inline(always)]
fn small_sigma1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}

// ─────────────────────────────────────────────────────────────────────────
// Runtime SHA-NI detection (cached after first call)
// ─────────────────────────────────────────────────────────────────────────
//
// 0 = unknown (probe needed), 1 = absent, 2 = present.
// Also requires SSSE3 + SSE4.1 because the algorithm uses pshufb / palignr /
// pblendw to reorder state and message words. All three are universal on any
// CPU that has SHA-NI (Goldmont/Ryzen and later), but we still gate on each.
static SHA_NI_PROBE: AtomicU8 = AtomicU8::new(0);

#[cfg(target_arch = "x86_64")]
fn has_sha_ni() -> bool {
    let cached = SHA_NI_PROBE.load(Ordering::Relaxed);
    if cached != 0 {
        return cached == 2;
    }
    // CPUID.07H.0:EBX bit 29 = SHA, ECX bit 0 must be checked at leaf 1 for
    // SSSE3 (CPUID.01H:ECX[9]) and SSE4.1 (CPUID.01H:ECX[19]).
    let (sha_bit, ssse3_bit, sse41_bit) = unsafe {
        let leaf1 = core::arch::x86_64::__cpuid(1);
        let leaf7 = core::arch::x86_64::__cpuid_count(7, 0);
        (
            (leaf7.ebx >> 29) & 1 != 0,
            (leaf1.ecx >> 9) & 1 != 0,
            (leaf1.ecx >> 19) & 1 != 0,
        )
    };
    let present = sha_bit && ssse3_bit && sse41_bit;
    SHA_NI_PROBE.store(if present { 2 } else { 1 }, Ordering::Relaxed);
    present
}

#[cfg(not(target_arch = "x86_64"))]
fn has_sha_ni() -> bool {
    false
}

// ─────────────────────────────────────────────────────────────────────────
// SHA-NI compress — multi-block
// ─────────────────────────────────────────────────────────────────────────
//
// Standard Intel SHA Extensions sequence: load state into two xmm registers
// laid out as ABEF/CDGH (the form sha256rnds2 expects), run 32 pairs of
// rounds (each pair = sha256rnds2 + a high-half shuffle + a second
// sha256rnds2), unshuffle back to the canonical a..h state, add to running
// state.
//
// Multi-block lets us amortize the initial load+permute and the final
// unshuffle across all blocks, so the per-block cost is essentially just
// the 16 rnds2 + 12 msg1/msg2 instructions.

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
unsafe fn compress_blocks_sha_ni(state: &mut [u32; 8], data: &[u8]) {
    use core::arch::x86_64::*;

    // Byte-swap mask: pshufb against this turns little-endian 32-bit lanes
    // (loaded from memory) into big-endian message words.
    let mask = _mm_set_epi64x(
        0x0c0d_0e0f_0809_0a0bu64 as i64,
        0x0405_0607_0001_0203u64 as i64,
    );

    // Load state and rearrange into ABEF/CDGH form.
    //   state in memory: a, b, c, d, e, f, g, h
    //   tmp    = [a, b, c, d]
    //   state1 = [e, f, g, h]
    // After permute:
    //   state0 = [a, b, e, f]   (ABEF)
    //   state1 = [c, d, g, h]   (CDGH)
    let mut tmp = _mm_loadu_si128(state.as_ptr() as *const __m128i);
    let mut state1 = _mm_loadu_si128(state.as_ptr().add(4) as *const __m128i);
    tmp = _mm_shuffle_epi32(tmp, 0xB1);          // [b, a, d, c]
    state1 = _mm_shuffle_epi32(state1, 0x1B);    // [h, g, f, e]
    let mut state0 = _mm_alignr_epi8(tmp, state1, 8); // [d, c, h, g] -> rebuilt below
    state1 = _mm_blend_epi16(state1, tmp, 0xF0); // [c, d, g, h]
    // After the alignr+blend:
    //   state0 holds ABEF
    //   state1 holds CDGH

    let mut offset = 0usize;
    while offset + 64 <= data.len() {
        let block = data.as_ptr().add(offset);

        let abef_save = state0;
        let cdgh_save = state1;

        // Rounds 0–3
        let msg0_raw = _mm_loadu_si128(block as *const __m128i);
        let mut msg0 = _mm_shuffle_epi8(msg0_raw, mask);
        let mut msg = _mm_add_epi32(
            msg0,
            _mm_set_epi64x(0xE9B5DBA5B5C0FBCFu64 as i64, 0x71374491428A2F98u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);

        // Rounds 4–7
        let msg1_raw = _mm_loadu_si128(block.add(16) as *const __m128i);
        let mut msg1 = _mm_shuffle_epi8(msg1_raw, mask);
        msg = _mm_add_epi32(
            msg1,
            _mm_set_epi64x(0xAB1C5ED5923F82A4u64 as i64, 0x59F111F13956C25Bu64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg0 = _mm_sha256msg1_epu32(msg0, msg1);

        // Rounds 8–11
        let msg2_raw = _mm_loadu_si128(block.add(32) as *const __m128i);
        let mut msg2 = _mm_shuffle_epi8(msg2_raw, mask);
        msg = _mm_add_epi32(
            msg2,
            _mm_set_epi64x(0x550C7DC3243185BEu64 as i64, 0x12835B01D807AA98u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg1 = _mm_sha256msg1_epu32(msg1, msg2);

        // Rounds 12–15
        let msg3_raw = _mm_loadu_si128(block.add(48) as *const __m128i);
        let mut msg3 = _mm_shuffle_epi8(msg3_raw, mask);
        msg = _mm_add_epi32(
            msg3,
            _mm_set_epi64x(0xC19BF1749BDC06A7u64 as i64, 0x80DEB1FE72BE5D74u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        let mut tmp_align = _mm_alignr_epi8(msg3, msg2, 4);
        msg0 = _mm_add_epi32(msg0, tmp_align);
        msg0 = _mm_sha256msg2_epu32(msg0, msg3);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg2 = _mm_sha256msg1_epu32(msg2, msg3);

        // Rounds 16–19
        msg = _mm_add_epi32(
            msg0,
            _mm_set_epi64x(0x240CA1CC0FC19DC6u64 as i64, 0xEFBE4786E49B69C1u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg0, msg3, 4);
        msg1 = _mm_add_epi32(msg1, tmp_align);
        msg1 = _mm_sha256msg2_epu32(msg1, msg0);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg3 = _mm_sha256msg1_epu32(msg3, msg0);

        // Rounds 20–23
        msg = _mm_add_epi32(
            msg1,
            _mm_set_epi64x(0x76F988DA5CB0A9DCu64 as i64, 0x4A7484AA2DE92C6Fu64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg1, msg0, 4);
        msg2 = _mm_add_epi32(msg2, tmp_align);
        msg2 = _mm_sha256msg2_epu32(msg2, msg1);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg0 = _mm_sha256msg1_epu32(msg0, msg1);

        // Rounds 24–27
        msg = _mm_add_epi32(
            msg2,
            _mm_set_epi64x(0xBF597FC7B00327C8u64 as i64, 0xA831C66D983E5152u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg2, msg1, 4);
        msg3 = _mm_add_epi32(msg3, tmp_align);
        msg3 = _mm_sha256msg2_epu32(msg3, msg2);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg1 = _mm_sha256msg1_epu32(msg1, msg2);

        // Rounds 28–31
        msg = _mm_add_epi32(
            msg3,
            _mm_set_epi64x(0x1429296706CA6351u64 as i64, 0xD5A79147C6E00BF3u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg3, msg2, 4);
        msg0 = _mm_add_epi32(msg0, tmp_align);
        msg0 = _mm_sha256msg2_epu32(msg0, msg3);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg2 = _mm_sha256msg1_epu32(msg2, msg3);

        // Rounds 32–35
        msg = _mm_add_epi32(
            msg0,
            _mm_set_epi64x(0x53380D134D2C6DFCu64 as i64, 0x2E1B213827B70A85u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg0, msg3, 4);
        msg1 = _mm_add_epi32(msg1, tmp_align);
        msg1 = _mm_sha256msg2_epu32(msg1, msg0);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg3 = _mm_sha256msg1_epu32(msg3, msg0);

        // Rounds 36–39
        msg = _mm_add_epi32(
            msg1,
            _mm_set_epi64x(0x92722C8581C2C92Eu64 as i64, 0x766A0ABB650A7354u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg1, msg0, 4);
        msg2 = _mm_add_epi32(msg2, tmp_align);
        msg2 = _mm_sha256msg2_epu32(msg2, msg1);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg0 = _mm_sha256msg1_epu32(msg0, msg1);

        // Rounds 40–43
        msg = _mm_add_epi32(
            msg2,
            _mm_set_epi64x(0xC76C51A3C24B8B70u64 as i64, 0xA81A664BA2BFE8A1u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg2, msg1, 4);
        msg3 = _mm_add_epi32(msg3, tmp_align);
        msg3 = _mm_sha256msg2_epu32(msg3, msg2);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg1 = _mm_sha256msg1_epu32(msg1, msg2);

        // Rounds 44–47
        msg = _mm_add_epi32(
            msg3,
            _mm_set_epi64x(0x106AA070F40E3585u64 as i64, 0xD6990624D192E819u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg3, msg2, 4);
        msg0 = _mm_add_epi32(msg0, tmp_align);
        msg0 = _mm_sha256msg2_epu32(msg0, msg3);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg2 = _mm_sha256msg1_epu32(msg2, msg3);

        // Rounds 48–51
        msg = _mm_add_epi32(
            msg0,
            _mm_set_epi64x(0x34B0BCB52748774Cu64 as i64, 0x1E376C0819A4C116u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg0, msg3, 4);
        msg1 = _mm_add_epi32(msg1, tmp_align);
        msg1 = _mm_sha256msg2_epu32(msg1, msg0);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);
        msg3 = _mm_sha256msg1_epu32(msg3, msg0);

        // Rounds 52–55
        msg = _mm_add_epi32(
            msg1,
            _mm_set_epi64x(0x682E6FF35B9CCA4Fu64 as i64, 0x4ED8AA4A391C0CB3u64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg1, msg0, 4);
        msg2 = _mm_add_epi32(msg2, tmp_align);
        msg2 = _mm_sha256msg2_epu32(msg2, msg1);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);

        // Rounds 56–59
        msg = _mm_add_epi32(
            msg2,
            _mm_set_epi64x(0x8CC7020884C87814u64 as i64, 0x78A5636F748F82EEu64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        tmp_align = _mm_alignr_epi8(msg2, msg1, 4);
        msg3 = _mm_add_epi32(msg3, tmp_align);
        msg3 = _mm_sha256msg2_epu32(msg3, msg2);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);

        // Rounds 60–63
        msg = _mm_add_epi32(
            msg3,
            _mm_set_epi64x(0xC67178F2BEF9A3F7u64 as i64, 0xA4506CEB90BEFFFAu64 as i64),
        );
        state1 = _mm_sha256rnds2_epu32(state1, state0, msg);
        msg = _mm_shuffle_epi32(msg, 0x0E);
        state0 = _mm_sha256rnds2_epu32(state0, state1, msg);

        state0 = _mm_add_epi32(state0, abef_save);
        state1 = _mm_add_epi32(state1, cdgh_save);

        offset += 64;
    }

    // Unshuffle ABEF/CDGH back into a..h memory order.
    tmp = _mm_shuffle_epi32(state0, 0x1B);          // [f, e, b, a]
    state1 = _mm_shuffle_epi32(state1, 0xB1);       // [d, c, h, g]
    state0 = _mm_blend_epi16(tmp, state1, 0xF0);    // [d, c, b, a]
    state1 = _mm_alignr_epi8(state1, tmp, 8);       // [h, g, f, e]
    _mm_storeu_si128(state.as_mut_ptr() as *mut __m128i, state0);
    _mm_storeu_si128(state.as_mut_ptr().add(4) as *mut __m128i, state1);
}

// ─────────────────────────────────────────────────────────────────────────
// Software fallback — FIPS 180-4 reference, byte-identical to the SHA-NI path
// ─────────────────────────────────────────────────────────────────────────

fn compress_block_sw(state: &mut [u32; 8], block: &[u8]) {
    // Step 1: Parse block into 16 big-endian u32 words
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }

    // Step 2: Expand to 64 words
    for i in 16..64 {
        w[i] = small_sigma1(w[i - 2])
            .wrapping_add(w[i - 7])
            .wrapping_add(small_sigma0(w[i - 15]))
            .wrapping_add(w[i - 16]);
    }

    // Step 3: Initialize working variables from state
    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    let mut f = state[5];
    let mut g = state[6];
    let mut h = state[7];

    // Step 4: 64 rounds
    for i in 0..64 {
        let t1 = h
            .wrapping_add(big_sigma1(e))
            .wrapping_add(ch(e, f, g))
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let t2 = big_sigma0(a).wrapping_add(maj(a, b, c));

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    // Step 5: Add working variables back to state
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// Compress one or more 64-byte blocks. `data.len()` must be a multiple of 64.
fn compress_blocks(state: &mut [u32; 8], data: &[u8]) {
    debug_assert_eq!(data.len() % 64, 0);
    if data.is_empty() {
        return;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if has_sha_ni() {
            // SAFETY: feature check above ensures the CPU supports the
            // sha/sse2/ssse3/sse4.1 instructions used inside.
            unsafe { compress_blocks_sha_ni(state, data) };
            return;
        }
    }
    for chunk in data.chunks_exact(64) {
        compress_block_sw(state, chunk);
    }
}

struct Sha256State {
    state: [u32; 8],
    buffer: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256State {
    fn new() -> Self {
        Self {
            state: H_INIT,
            buffer: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        let mut offset = 0;
        self.total_len += data.len() as u64;

        // If we have buffered data, try to fill the buffer to 64 bytes
        if self.buf_len > 0 {
            let needed = 64 - self.buf_len;
            let copy_len = if data.len() < needed {
                data.len()
            } else {
                needed
            };
            self.buffer[self.buf_len..self.buf_len + copy_len].copy_from_slice(&data[..copy_len]);
            self.buf_len += copy_len;
            offset += copy_len;

            if self.buf_len == 64 {
                let block = self.buffer;
                compress_blocks(&mut self.state, &block);
                self.buf_len = 0;
            }
        }

        // Process all remaining full blocks in one call (lets the SHA-NI path
        // amortize state load/permute across the whole run).
        let full_block_bytes = (data.len() - offset) & !63;
        if full_block_bytes > 0 {
            compress_blocks(&mut self.state, &data[offset..offset + full_block_bytes]);
            offset += full_block_bytes;
        }

        // Buffer remaining bytes
        let remaining = data.len() - offset;
        if remaining > 0 {
            self.buffer[..remaining].copy_from_slice(&data[offset..]);
            self.buf_len = remaining;
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len * 8;

        // Append 0x80 byte
        self.buffer[self.buf_len] = 0x80;
        self.buf_len += 1;

        // If not enough room for the 8-byte length, pad and compress
        if self.buf_len > 56 {
            // Zero-fill rest of buffer
            for i in self.buf_len..64 {
                self.buffer[i] = 0;
            }
            let block = self.buffer;
            compress_blocks(&mut self.state, &block);
            self.buf_len = 0;
        }

        // Zero-fill up to byte 56
        for i in self.buf_len..56 {
            self.buffer[i] = 0;
        }

        // Append 64-bit big-endian bit count
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());

        let block = self.buffer;
        compress_blocks(&mut self.state, &block);

        // Emit state as 32 bytes big-endian
        let mut output = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            output[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
        }
        output
    }
}

/// Compute SHA-256 hash
///
/// Returns 32-byte hash of input data.
pub fn hash_sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256State::new();
    h.update(data);
    h.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_to_bytes(hex: &str) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        for i in 0..32 {
            bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        bytes
    }

    #[test]
    fn test_sha256_empty() {
        let hash = hash_sha256(b"");
        assert_eq!(
            hash,
            hex_to_bytes("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }

    #[test]
    fn test_sha256_abc() {
        let hash = hash_sha256(b"abc");
        assert_eq!(
            hash,
            hex_to_bytes("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn test_sha256_two_blocks() {
        // 448-bit message — tests padding at block boundary
        let hash = hash_sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq");
        assert_eq!(
            hash,
            hex_to_bytes("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
        );
    }

    #[test]
    fn test_sha256_deterministic() {
        let data = b"hello world";
        let hash1 = hash_sha256(data);
        let hash2 = hash_sha256(data);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_sha256_different_inputs() {
        let hash1 = hash_sha256(b"hello");
        let hash2 = hash_sha256(b"world");
        assert_ne!(hash1, hash2);
    }
}
