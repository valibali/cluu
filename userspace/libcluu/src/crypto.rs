//! Cryptographic utilities for userspace services.
//!
//! Provides SHA-256 hashing and salted password hashing/verification.

use alloc::string::String;

// ---------------------------------------------------------------------------
// SHA-256 (FIPS 180-4) — stack-only, no heap
// ---------------------------------------------------------------------------

const H_INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
    0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
    0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
    0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[inline(always)]
fn ch(x: u32, y: u32, z: u32) -> u32 { (x & y) ^ (!x & z) }

#[inline(always)]
fn maj(x: u32, y: u32, z: u32) -> u32 { (x & y) ^ (x & z) ^ (y & z) }

#[inline(always)]
fn big_sigma0(x: u32) -> u32 { x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22) }

#[inline(always)]
fn big_sigma1(x: u32) -> u32 { x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25) }

#[inline(always)]
fn small_sigma0(x: u32) -> u32 { x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3) }

#[inline(always)]
fn small_sigma1(x: u32) -> u32 { x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10) }

struct Sha256State {
    state: [u32; 8],
    buffer: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256State {
    fn new() -> Self {
        Self { state: H_INIT, buffer: [0u8; 64], buf_len: 0, total_len: 0 }
    }

    fn update(&mut self, data: &[u8]) {
        let mut offset = 0;
        self.total_len += data.len() as u64;

        if self.buf_len > 0 {
            let needed = 64 - self.buf_len;
            let copy_len = data.len().min(needed);
            self.buffer[self.buf_len..self.buf_len + copy_len]
                .copy_from_slice(&data[..copy_len]);
            self.buf_len += copy_len;
            offset += copy_len;

            if self.buf_len == 64 {
                let block = self.buffer;
                Self::compress(&mut self.state, &block);
                self.buf_len = 0;
            }
        }

        while offset + 64 <= data.len() {
            Self::compress(&mut self.state, &data[offset..offset + 64]);
            offset += 64;
        }

        let remaining = data.len() - offset;
        if remaining > 0 {
            self.buffer[..remaining].copy_from_slice(&data[offset..]);
            self.buf_len = remaining;
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len * 8;
        self.buffer[self.buf_len] = 0x80;
        self.buf_len += 1;

        if self.buf_len > 56 {
            for i in self.buf_len..64 { self.buffer[i] = 0; }
            Self::compress(&mut self.state, &self.buffer);
            self.buf_len = 0;
        }

        for i in self.buf_len..56 { self.buffer[i] = 0; }
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        Self::compress(&mut self.state, &self.buffer);

        let mut output = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            output[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
        }
        output
    }

    fn compress(state: &mut [u32; 8], block: &[u8]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            w[i] = small_sigma1(w[i - 2])
                .wrapping_add(w[i - 7])
                .wrapping_add(small_sigma0(w[i - 15]))
                .wrapping_add(w[i - 16]);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;

        for i in 0..64 {
            let t1 = h.wrapping_add(big_sigma1(e))
                .wrapping_add(ch(e, f, g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = big_sigma0(a).wrapping_add(maj(a, b, c));
            h = g; g = f; f = e; e = d.wrapping_add(t1);
            d = c; c = b; b = a; a = t1.wrapping_add(t2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }
}

/// Compute SHA-256 hash of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256State::new();
    h.update(data);
    h.finalize()
}

// ---------------------------------------------------------------------------
// Hex encoding/decoding helpers
// ---------------------------------------------------------------------------

const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

fn bytes_to_hex(bytes: &[u8], buf: &mut [u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        buf[i * 2] = HEX_CHARS[(b >> 4) as usize];
        buf[i * 2 + 1] = HEX_CHARS[(b & 0x0f) as usize];
    }
}

fn hex_to_byte(hi: u8, lo: u8) -> Option<u8> {
    let h = match hi {
        b'0'..=b'9' => hi - b'0',
        b'a'..=b'f' => hi - b'a' + 10,
        b'A'..=b'F' => hi - b'A' + 10,
        _ => return None,
    };
    let l = match lo {
        b'0'..=b'9' => lo - b'0',
        b'a'..=b'f' => lo - b'a' + 10,
        b'A'..=b'F' => lo - b'A' + 10,
        _ => return None,
    };
    Some((h << 4) | l)
}

fn hex_decode(hex: &str, out: &mut [u8]) -> bool {
    let bytes = hex.as_bytes();
    if bytes.len() != out.len() * 2 { return false; }
    for i in 0..out.len() {
        match hex_to_byte(bytes[i * 2], bytes[i * 2 + 1]) {
            Some(b) => out[i] = b,
            None => return false,
        }
    }
    true
}

// ---------------------------------------------------------------------------
// RDRAND-based random salt generation
// ---------------------------------------------------------------------------

/// Pull a single random u64 from RDRAND. Retries up to 10 times (per Intel's
/// guidance). Returns `None` if RDRAND never signals success — caller must
/// NOT fall back to a zero value, which would produce a predictable salt.
fn rdrand_u64() -> Option<u64> {
    for _ in 0..10 {
        let val: u64;
        let ok: u8;
        unsafe {
            core::arch::asm!(
                "rdrand {val}",
                "setc {ok}",
                val = out(reg) val,
                ok = out(reg_byte) ok,
                options(nomem, nostack),
            );
        }
        if ok != 0 {
            return Some(val);
        }
    }
    None
}

/// Generate 16 bytes of random data using x86_64 RDRAND.
///
/// Returns `None` if RDRAND fails repeatedly (hardware fault, disabled via
/// firmware, or hypervisor not advertising RDRAND). A failure here is
/// security-critical: callers must refuse to produce a password hash with a
/// zero salt, which would collapse all passwords to a single deterministic
/// digest. The failure is logged loudly before returning None.
fn rdrand_salt() -> Option<[u8; 16]> {
    let r1 = match rdrand_u64() {
        Some(v) => v,
        None => {
            let _ = crate::debug_print("crypto: RDRAND failed after 10 retries (word 0)\n");
            return None;
        }
    };
    let r2 = match rdrand_u64() {
        Some(v) => v,
        None => {
            let _ = crate::debug_print("crypto: RDRAND failed after 10 retries (word 1)\n");
            return None;
        }
    };
    let mut salt = [0u8; 16];
    salt[..8].copy_from_slice(&r1.to_le_bytes());
    salt[8..].copy_from_slice(&r2.to_le_bytes());
    Some(salt)
}

// ---------------------------------------------------------------------------
// Password hashing: $sha256$<hex_salt>$<hex_hash>
// ---------------------------------------------------------------------------

/// Hash a password with a random salt.
///
/// Returns a string in the format `$sha256$<32-char-hex-salt>$<64-char-hex-hash>`.
/// The hash is computed as SHA-256(salt ‖ password).
///
/// Returns `None` if the platform RDRAND is unavailable or persistently failing.
/// Callers MUST refuse to set the password in that case — producing a hash with
/// a zero salt would make all stored hashes trivially cross-correlatable.
pub fn hash_password(password: &str) -> Option<String> {
    let salt = rdrand_salt()?;
    Some(hash_password_with_salt(password, &salt))
}

/// Hash a password with the given 16-byte salt.
fn hash_password_with_salt(password: &str, salt: &[u8; 16]) -> String {
    let mut hasher = Sha256State::new();
    hasher.update(salt);
    hasher.update(password.as_bytes());
    let digest = hasher.finalize();

    // Format: $sha256$<32 hex chars salt>$<64 hex chars hash>
    // Total: 7 + 32 + 1 + 64 = 104 chars
    let mut result = String::with_capacity(104);
    result.push_str("$sha256$");

    let mut salt_hex = [0u8; 32];
    bytes_to_hex(salt, &mut salt_hex);
    // SAFETY: hex chars are always valid UTF-8
    result.push_str(unsafe { core::str::from_utf8_unchecked(&salt_hex) });

    result.push('$');

    let mut hash_hex = [0u8; 64];
    bytes_to_hex(&digest, &mut hash_hex);
    result.push_str(unsafe { core::str::from_utf8_unchecked(&hash_hex) });

    result
}

/// Verify a password against a stored hash string.
///
/// Supports two formats:
/// - `$sha256$<salt_hex>$<hash_hex>` — salted SHA-256 (preferred)
/// - Any other string — treated as legacy plaintext (for migration)
///
/// Empty stored passwords always match (no password required).
pub fn verify_password(password: &str, stored: &str) -> bool {
    if stored.is_empty() {
        return true;
    }

    if let Some(rest) = stored.strip_prefix("$sha256$") {
        // Parse salt and hash
        let mut parts = rest.splitn(2, '$');
        let salt_hex = match parts.next() {
            Some(s) if s.len() == 32 => s,
            _ => return false,
        };
        let hash_hex = match parts.next() {
            Some(s) if s.len() == 64 => s,
            _ => return false,
        };

        let mut salt = [0u8; 16];
        if !hex_decode(salt_hex, &mut salt) { return false; }

        let mut expected = [0u8; 32];
        if !hex_decode(hash_hex, &mut expected) { return false; }

        // Recompute hash
        let mut hasher = Sha256State::new();
        hasher.update(&salt);
        hasher.update(password.as_bytes());
        let computed = hasher.finalize();

        // Constant-time comparison
        ct_eq(&computed, &expected)
    } else {
        // Legacy plaintext comparison (migration support)
        ct_eq(password.as_bytes(), stored.as_bytes())
    }
}

/// Constant-time byte array comparison.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}
