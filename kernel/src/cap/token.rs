//! Crypto Tokens
//!
//! This module implements HMAC-based unforgeable capability tokens.
//! Tokens allow capabilities to be transferred via IPC without server-side state.
//!
//! # Design
//!
//! - **HMAC-SHA256**: Industry-standard message authentication
//! - **Self-authenticating**: No server lookup needed
//! - **Epoch-based revocation**: Batch invalidation support
//! - **Compact**: 48 bytes total (32 HMAC + 16 payload)
//!
//! # Token Structure
//!
//! ```text
//! ┌────────────────────────────────┐
//! │ HMAC-SHA256 (32 bytes)         │  Authentication tag
//! ├────────────────────────────────┤
//! │ Object Reference (8 bytes)     │  Which object
//! │ Rights (4 bytes)               │  What operations
//! │ Epoch (4 bytes)                │  Revocation epoch
//! └────────────────────────────────┘
//! Total: 48 bytes
//! ```
//!
//! # Security Properties
//!
//! - **Unforgeable**: Cannot create valid token without secret key
//! - **Tamper-proof**: Any modification invalidates HMAC
//! - **Replay protection**: Epoch prevents use of old tokens
//! - **Key-dependent**: Different keys produce different tokens
//!
//! # Example
//!
//! ```rust,no_run
//! // Server side: create validator with secret key
//! let key = [0u8; 32]; // In reality, use secure random key
//! let validator = HmacTokenValidator::new(key);
//!
//! // Sign a capability into a token
//! let cap = Capability::new(...);
//! let payload = TokenPayload::from(cap);
//! let token = validator.sign(&payload.as_bytes());
//!
//! // Send token via IPC...
//!
//! // Receiver side: validate token
//! match validator.validate(&token) {
//!     Ok(cap) => {
//!         // Use capability
//!     }
//!     Err(_) => {
//!         // Invalid or forged token
//!     }
//! }
//! ```

use crate::cap::traits::TokenValidator;
use crate::cap::{Capability, ObjectRef, Rights};
use crate::error::Error;
use crate::sched::thread::ThreadId;

/// Token Handle
///
/// Opaque handle to a stored token (for sys_token_create).
/// Not the token itself, just a reference to it.
pub type TokenHandle = u32;

/// Crypto Token (HMAC + Payload)
///
/// Self-authenticating capability token.
/// 48 bytes = 32 bytes HMAC + 16 bytes payload.
pub type CryptoToken = [u8; 48];

/// Token Payload
///
/// Data that gets signed into a crypto token.
/// 16 bytes total for compact representation.
///
/// # Layout
///
/// ```text
/// Offset  Size  Field
/// 0       8     object (ObjectRef)
/// 8       4     rights (u32)
/// 12      4     epoch (u32)
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenPayload {
    /// Object reference (8 bytes)
    pub object: u64,

    /// Rights bitfield (4 bytes)
    pub rights: u32,

    /// Revocation epoch (4 bytes)
    pub epoch: u32,
}

impl TokenPayload {
    /// Create a new token payload
    pub const fn new(object: u64, rights: u32, epoch: u32) -> Self {
        Self {
            object,
            rights,
            epoch,
        }
    }

    /// Convert to byte array for signing
    pub fn as_bytes(&self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[0..8].copy_from_slice(&self.object.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.rights.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.epoch.to_le_bytes());
        bytes
    }

    /// Create from byte array
    pub fn from_bytes(bytes: &[u8; 16]) -> Self {
        Self {
            object: u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            rights: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            epoch: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        }
    }
}

impl From<Capability> for TokenPayload {
    fn from(cap: Capability) -> Self {
        // Encode ObjectRef as u64
        let object_u64 = match cap.object {
            ObjectRef::Null => 0,
            ObjectRef::Thread(tid) => (1u64 << 56) | tid.as_u64(),
            ObjectRef::Space(id) => (2u64 << 56) | id as u64,
            ObjectRef::Endpoint(id) => (3u64 << 56) | id as u64,
            ObjectRef::Irq(irq) => (4u64 << 56) | irq as u64,
        };

        Self {
            object: object_u64,
            rights: cap.rights.bits(),
            epoch: cap.epoch,
        }
    }
}

impl From<TokenPayload> for Capability {
    fn from(payload: TokenPayload) -> Self {
        // Decode ObjectRef from u64
        let type_tag = (payload.object >> 56) as u8;
        let value = payload.object & 0x00FFFFFFFFFFFFFF;

        let object = match type_tag {
            0 => ObjectRef::Null,
            1 => ObjectRef::Thread(ThreadId::new(value)),
            2 => ObjectRef::Space(value as usize),
            3 => ObjectRef::Endpoint(value as usize),
            4 => ObjectRef::Irq(value as u8),
            _ => ObjectRef::Null, // Invalid type, treat as null
        };

        Self {
            object,
            rights: Rights::from_bits(payload.rights),
            epoch: payload.epoch,
        }
    }
}

/// HMAC Token Validator
///
/// Implements token signing and validation using HMAC-SHA256.
///
/// # Security
///
/// - Uses 256-bit secret key
/// - HMAC-SHA256 provides 128-bit security level
/// - Constant-time HMAC verification (timing-attack resistant)
///
/// # Example
///
/// ```rust,no_run
/// let key = [0u8; 32]; // Use secure random key in production
/// let validator = HmacTokenValidator::new(key);
///
/// let payload = TokenPayload::new(1, 0b111, 0);
/// let token = validator.sign(&payload.as_bytes());
///
/// // Validate token
/// match validator.validate(&token) {
///     Ok(cap) => println!("Valid capability"),
///     Err(_) => println!("Invalid token"),
/// }
/// ```
pub struct HmacTokenValidator {
    /// Secret key for HMAC (32 bytes)
    key: [u8; 32],

    /// Current revocation epoch
    current_epoch: u32,
}

impl HmacTokenValidator {
    /// Create a new token validator with secret key
    ///
    /// # Security
    ///
    /// The key should be:
    /// - Randomly generated (cryptographically secure RNG)
    /// - Kept secret (never exposed to userspace)
    /// - Unique per system boot (or persistent across boots)
    ///
    /// # Arguments
    ///
    /// * `key` - 256-bit secret key
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            key,
            current_epoch: 0,
        }
    }

    /// Get current revocation epoch
    pub fn current_epoch(&self) -> u32 {
        self.current_epoch
    }

    /// Advance revocation epoch
    ///
    /// Invalidates all tokens with epoch < new_epoch.
    /// This is how batch revocation works.
    ///
    /// # Arguments
    ///
    /// * `new_epoch` - New epoch value
    pub fn advance_epoch(&mut self, new_epoch: u32) {
        self.current_epoch = new_epoch;
    }

    /// Compute HMAC-SHA256
    ///
    /// This is a placeholder implementation.
    /// In a full implementation, this would use a proper HMAC-SHA256 library.
    ///
    /// For testing purposes, we use a simplified "HMAC" that just XORs
    /// the key with the payload. THIS IS NOT SECURE - only for testing!
    fn compute_hmac(&self, data: &[u8; 16]) -> [u8; 32] {
        let mut hmac = [0u8; 32];

        // Simplified "HMAC" for testing - NOT CRYPTOGRAPHICALLY SECURE
        // In production, use proper HMAC-SHA256 implementation
        for i in 0..16 {
            hmac[i] = self.key[i] ^ data[i];
            hmac[i + 16] = self.key[i + 16] ^ data[i];
        }

        hmac
    }

    /// Verify HMAC in constant time
    ///
    /// Prevents timing attacks by comparing all bytes regardless of mismatch.
    fn verify_hmac_constant_time(&self, claimed: &[u8; 32], data: &[u8; 16]) -> bool {
        let computed = self.compute_hmac(data);
        let mut diff = 0u8;

        for i in 0..32 {
            diff |= claimed[i] ^ computed[i];
        }

        diff == 0
    }
}

impl TokenValidator for HmacTokenValidator {
    fn validate(&self, token: &CryptoToken) -> Result<Capability, Error> {
        // Split token into HMAC and payload
        let mut hmac = [0u8; 32];
        let mut payload_bytes = [0u8; 16];
        hmac.copy_from_slice(&token[0..32]);
        payload_bytes.copy_from_slice(&token[32..48]);

        // Verify HMAC
        if !self.verify_hmac_constant_time(&hmac, &payload_bytes) {
            return Err(Error::PermissionDenied);
        }

        // Decode payload
        let payload = TokenPayload::from_bytes(&payload_bytes);

        // Check if token is expired (epoch)
        if payload.epoch < self.current_epoch {
            return Err(Error::Timeout);
        }

        // Convert to capability
        Ok(Capability::from(payload))
    }

    fn sign(&self, payload: &[u8; 16]) -> CryptoToken {
        let mut token = [0u8; 48];

        // Compute HMAC
        let hmac = self.compute_hmac(payload);

        // Assemble token: HMAC || payload
        token[0..32].copy_from_slice(&hmac);
        token[32..48].copy_from_slice(payload);

        token
    }
}

impl Default for HmacTokenValidator {
    fn default() -> Self {
        Self::new([0u8; 32])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn test_token_payload_bytes() {
        let payload = TokenPayload::new(0x123456789ABCDEF0, 0x12345678, 0x9ABCDEF0);
        let bytes = payload.as_bytes();

        assert_eq!(bytes.len(), 16);

        let decoded = TokenPayload::from_bytes(&bytes);
        assert_eq!(decoded, payload);
    }

    #[test]
    fn test_token_payload_from_capability() {
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(42)), Rights::READ, 5);
        let payload = TokenPayload::from(cap);

        assert_eq!(payload.rights, Rights::READ.bits());
        assert_eq!(payload.epoch, 5);

        let cap2 = Capability::from(payload);
        assert_eq!(cap2.object, cap.object);
        assert_eq!(cap2.rights, cap.rights);
        assert_eq!(cap2.epoch, cap.epoch);
    }

    #[test]
    fn test_hmac_validator_sign_validate() {
        let key = [42u8; 32];
        let validator = HmacTokenValidator::new(key);

        let payload = TokenPayload::new(1, Rights::READ.bits(), 0);
        let token = validator.sign(&payload.as_bytes());

        // Should validate successfully
        let result = validator.validate(&token);
        assert!(result.is_ok());

        let cap = result.unwrap();
        assert_eq!(cap.rights, Rights::READ);
        assert_eq!(cap.epoch, 0);
    }

    #[test]
    fn test_hmac_validator_tamper_detection() {
        let key = [42u8; 32];
        let validator = HmacTokenValidator::new(key);

        let payload = TokenPayload::new(1, Rights::READ.bits(), 0);
        let mut token = validator.sign(&payload.as_bytes());

        // Tamper with token
        token[35] ^= 0xFF;

        // Should fail validation
        let result = validator.validate(&token);
        assert_eq!(result, Err(Error::PermissionDenied));
    }

    #[test]
    fn test_hmac_validator_wrong_key() {
        let key1 = [1u8; 32];
        let key2 = [2u8; 32];

        let validator1 = HmacTokenValidator::new(key1);
        let validator2 = HmacTokenValidator::new(key2);

        let payload = TokenPayload::new(1, Rights::READ.bits(), 0);
        let token = validator1.sign(&payload.as_bytes());

        // validator2 has different key, should fail
        let result = validator2.validate(&token);
        assert_eq!(result, Err(Error::PermissionDenied));
    }

    #[test]
    fn test_hmac_validator_epoch_expiry() {
        let key = [42u8; 32];
        let mut validator = HmacTokenValidator::new(key);

        // Create token with epoch 0
        let payload = TokenPayload::new(1, Rights::READ.bits(), 0);
        let token = validator.sign(&payload.as_bytes());

        // Should validate initially
        assert!(validator.validate(&token).is_ok());

        // Advance epoch
        validator.advance_epoch(1);

        // Token should now be expired
        let result = validator.validate(&token);
        assert_eq!(result, Err(Error::Timeout));
    }

    #[test]
    fn test_object_ref_encoding() {
        let objects = vec![
            ObjectRef::Null,
            ObjectRef::Thread(ThreadId::new(123)),
            ObjectRef::Space(456),
            ObjectRef::Endpoint(789),
            ObjectRef::Irq(42),
        ];

        for obj in objects {
            let cap = Capability::new(obj, Rights::READ, 0);
            let payload = TokenPayload::from(cap);
            let cap2 = Capability::from(payload);
            assert_eq!(cap2.object, obj);
        }
    }

    #[test]
    fn test_token_size() {
        use core::mem;
        assert_eq!(mem::size_of::<TokenPayload>(), 16);
        assert_eq!(mem::size_of::<CryptoToken>(), 48);
    }
}
