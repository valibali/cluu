//! Token Signatures
//!
//! HMAC-SHA256 signatures for token authentication.
//!
//! The signature binds all authorization-relevant fields:
//! - scope (what object)
//! - role (what rights)
//! - issuer (who created it)
//! - expire_at (when it expires)
//!
//! This ensures no field can be modified without breaking the signature.

use super::{OpaqueScope, Rights, Issuer, Timestamp};
use klibcluu::crypto::hmac_sha256;

/// HMAC-SHA256 signature (32 bytes)
#[derive(Clone, Copy)]
pub struct Signature([u8; 32]);

impl Signature {
    /// Compute signature over token fields
    ///
    /// signature = HMAC-SHA256(scope || role || issuer || expire_at, secret)
    ///
    /// # Arguments
    ///
    /// * `scope` - Opaque object reference
    /// * `role` - Rights bitmask
    /// * `issuer` - Token issuer
    /// * `expire_at` - Expiration timestamp
    /// * `secret` - Kernel secret key
    ///
    /// # Returns
    ///
    /// 32-byte HMAC signature
    pub fn compute(
        scope: OpaqueScope,
        role: Rights,
        issuer: Issuer,
        expire_at: Timestamp,
        secret: &[u8; 32],
    ) -> Self {
        // Serialize all fields into a single buffer
        let mut data = alloc::vec::Vec::with_capacity(16 + 4 + 8 + 8);

        // Scope (16 bytes)
        data.extend_from_slice(scope.as_bytes());

        // Role (4 bytes)
        data.extend_from_slice(&role.to_bytes());

        // Issuer (8 bytes)
        data.extend_from_slice(&issuer.to_bytes());

        // Expiration (8 bytes)
        data.extend_from_slice(&expire_at.as_u64().to_le_bytes());

        // Compute HMAC
        let hmac = hmac_sha256(secret, &data);

        Self(hmac)
    }

    /// Constant-time equality check
    ///
    /// Prevents timing attacks by ensuring comparison always takes
    /// the same amount of time regardless of where differences occur.
    pub fn constant_time_eq(&self, other: &Self) -> bool {
        let mut diff = 0u8;

        for i in 0..32 {
            diff |= self.0[i] ^ other.0[i];
        }

        diff == 0
    }

    /// Get raw bytes (for serialization)
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Create from bytes (for deserialization)
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl core::fmt::Debug for Signature {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Signature(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_deterministic() {
        let scope = OpaqueScope::from_bytes([1u8; 16]);
        let role = Rights::READ;
        let issuer = Issuer::Kernel;
        let expire_at = Timestamp::new(1000);
        let secret = [0x42u8; 32];

        let sig1 = Signature::compute(scope, role, issuer, expire_at, &secret);
        let sig2 = Signature::compute(scope, role, issuer, expire_at, &secret);

        assert!(sig1.constant_time_eq(&sig2));
    }

    #[test]
    fn test_signature_different_scope() {
        let scope1 = OpaqueScope::from_bytes([1u8; 16]);
        let scope2 = OpaqueScope::from_bytes([2u8; 16]);
        let role = Rights::READ;
        let issuer = Issuer::Kernel;
        let expire_at = Timestamp::new(1000);
        let secret = [0x42u8; 32];

        let sig1 = Signature::compute(scope1, role, issuer, expire_at, &secret);
        let sig2 = Signature::compute(scope2, role, issuer, expire_at, &secret);

        assert!(!sig1.constant_time_eq(&sig2));
    }

    #[test]
    fn test_signature_different_role() {
        let scope = OpaqueScope::from_bytes([1u8; 16]);
        let role1 = Rights::READ;
        let role2 = Rights::WRITE;
        let issuer = Issuer::Kernel;
        let expire_at = Timestamp::new(1000);
        let secret = [0x42u8; 32];

        let sig1 = Signature::compute(scope, role1, issuer, expire_at, &secret);
        let sig2 = Signature::compute(scope, role2, issuer, expire_at, &secret);

        assert!(!sig1.constant_time_eq(&sig2));
    }

    #[test]
    fn test_signature_different_secret() {
        let scope = OpaqueScope::from_bytes([1u8; 16]);
        let role = Rights::READ;
        let issuer = Issuer::Kernel;
        let expire_at = Timestamp::new(1000);
        let secret1 = [0x01u8; 32];
        let secret2 = [0x02u8; 32];

        let sig1 = Signature::compute(scope, role, issuer, expire_at, &secret1);
        let sig2 = Signature::compute(scope, role, issuer, expire_at, &secret2);

        assert!(!sig1.constant_time_eq(&sig2));
    }

    #[test]
    fn test_constant_time_eq() {
        let sig1 = Signature::from_bytes([1u8; 32]);
        let sig2 = Signature::from_bytes([1u8; 32]);
        let sig3 = Signature::from_bytes([2u8; 32]);

        assert!(sig1.constant_time_eq(&sig2));
        assert!(!sig1.constant_time_eq(&sig3));
    }
}
