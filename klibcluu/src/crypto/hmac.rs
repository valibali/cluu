//! HMAC-SHA256
//!
//! Hash-based Message Authentication Code using SHA-256.
//!
//! HMAC provides cryptographic authentication - only someone with
//! the secret key can create a valid HMAC.

use super::sha256::hash_sha256;

/// Compute HMAC-SHA256
///
/// # Arguments
///
/// * `key` - Secret key (32 bytes)
/// * `data` - Data to authenticate
///
/// # Returns
///
/// 32-byte HMAC
pub fn hmac_sha256(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64; // SHA-256 block size
    const IPAD: u8 = 0x36;
    const OPAD: u8 = 0x5C;

    // Prepare key (already 32 bytes, pad to 64)
    let mut key_block = [0u8; BLOCK_SIZE];
    key_block[..32].copy_from_slice(key);

    // Inner hash: H((K ⊕ ipad) || message)
    let mut inner_data = alloc::vec::Vec::with_capacity(BLOCK_SIZE + data.len());

    // K ⊕ ipad
    for i in 0..BLOCK_SIZE {
        inner_data.push(key_block[i] ^ IPAD);
    }

    // Append message
    inner_data.extend_from_slice(data);

    let inner_hash = hash_sha256(&inner_data);

    // Outer hash: H((K ⊕ opad) || inner_hash)
    let mut outer_data = alloc::vec::Vec::with_capacity(BLOCK_SIZE + 32);

    // K ⊕ opad
    for i in 0..BLOCK_SIZE {
        outer_data.push(key_block[i] ^ OPAD);
    }

    // Append inner hash
    outer_data.extend_from_slice(&inner_hash);

    hash_sha256(&outer_data)
}

/// Compute HMAC-SHA256 without heap allocation for small inputs.
///
/// Falls back to the allocating version when input exceeds MAX_DATA bytes.
pub fn hmac_sha256_fixed(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64; // SHA-256 block size
    const IPAD: u8 = 0x36;
    const OPAD: u8 = 0x5C;
    const MAX_DATA: usize = 128;

    if data.len() > MAX_DATA {
        return hmac_sha256(key, data);
    }

    // Prepare key (already 32 bytes, pad to 64)
    let mut key_block = [0u8; BLOCK_SIZE];
    key_block[..32].copy_from_slice(key);

    let mut inner_data = [0u8; BLOCK_SIZE + MAX_DATA];
    for i in 0..BLOCK_SIZE {
        inner_data[i] = key_block[i] ^ IPAD;
    }

    inner_data[BLOCK_SIZE..BLOCK_SIZE + data.len()].copy_from_slice(data);
    let inner_len = BLOCK_SIZE + data.len();
    let inner_hash = hash_sha256(&inner_data[..inner_len]);

    let mut outer_data = [0u8; BLOCK_SIZE + 32];
    for i in 0..BLOCK_SIZE {
        outer_data[i] = key_block[i] ^ OPAD;
    }
    outer_data[BLOCK_SIZE..BLOCK_SIZE + 32].copy_from_slice(&inner_hash);

    hash_sha256(&outer_data[..BLOCK_SIZE + 32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hmac_deterministic() {
        let key = [0x42u8; 32];
        let data = b"hello world";

        let hmac1 = hmac_sha256(&key, data);
        let hmac2 = hmac_sha256(&key, data);

        assert_eq!(hmac1, hmac2);
    }

    #[test]
    fn test_hmac_different_keys() {
        let key1 = [0x01u8; 32];
        let key2 = [0x02u8; 32];
        let data = b"hello world";

        let hmac1 = hmac_sha256(&key1, data);
        let hmac2 = hmac_sha256(&key2, data);

        assert_ne!(hmac1, hmac2);
    }

    #[test]
    fn test_hmac_different_data() {
        let key = [0x42u8; 32];

        let hmac1 = hmac_sha256(&key, b"hello");
        let hmac2 = hmac_sha256(&key, b"world");

        assert_ne!(hmac1, hmac2);
    }
}
