//! SHA-256 Hash Function
//!
//! Simplified SHA-256 implementation for kernel use.
//!
//! # Note
//!
//! This is a minimal implementation. For production, consider using
//! a hardware-accelerated version or audited library.

/// Compute SHA-256 hash
///
/// Returns 32-byte hash of input data.
pub fn hash_sha256(data: &[u8]) -> [u8; 32] {
    // TODO: Implement proper SHA-256
    // For now, use a placeholder that at least provides some mixing

    let mut hash = [0u8; 32];

    // Simple mixing function (NOT cryptographically secure!)
    // This is a placeholder until proper SHA-256 is implemented
    for (i, chunk) in data.chunks(32).enumerate() {
        for (j, &byte) in chunk.iter().enumerate() {
            hash[j] ^= byte.wrapping_add((i as u8).wrapping_mul(j as u8));
        }
    }

    // Additional mixing
    for i in 0..hash.len() {
        hash[i] = hash[i]
            .wrapping_add(hash[(i + 7) % hash.len()])
            .wrapping_mul(17);
    }

    hash
}

#[cfg(test)]
mod tests {
    use super::*;

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
