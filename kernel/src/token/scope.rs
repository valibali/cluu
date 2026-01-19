//! Opaque Scope - Non-enumerable Object References
//!
//! OpaqueScope provides unforgeable, non-enumerable references to kernel objects.
//!
//! # Security Properties
//!
//! 1. **Non-enumerable**: Random 128-bit values, cannot iterate to discover
//! 2. **Unforgeable**: Only kernel can create (requires internal mapping)
//! 3. **Unlinkable**: Same object gets different scope in different tokens
//!
//! # Design
//!
//! ```text
//! Userspace Token          Kernel Mapping
//! ┌─────────────────┐      ┌──────────────────────────┐
//! │ OpaqueScope:    │      │ OpaqueScope → ObjectRef  │
//! │ [a3 f2 ... 8d]  │ ───> │ [a3 f2...] → Thread(42)  │
//! │ (128 random bits)│     │ [7c 91...] → Space(17)   │
//! └─────────────────┘      └──────────────────────────┘
//! ```
//!
//! Prevents attacks where attacker tries sequential IDs to discover objects.

use core::fmt;

/// Opaque identifier for kernel objects
///
/// 128-bit random value that prevents enumeration attacks.
///
/// Properties:
/// - Non-enumerable: Cannot iterate to discover objects
/// - Unforgeable: Random value, not sequential
/// - Unlinkable: Same object can have different scopes in different tokens
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(align(16))]
pub struct OpaqueScope([u8; 16]);

impl OpaqueScope {
    /// Create from bytes (used during deserialization)
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Get raw bytes (for signature computation)
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Generate random opaque scope
    ///
    /// Uses kernel's cryptographic RNG to ensure unpredictability.
    ///
    /// # Note
    ///
    /// This requires the crypto/random module to be initialized.
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        klibcluu::crypto::fill_random(&mut bytes);
        Self(bytes)
    }

    /// Create from object reference (for kernel use)
    ///
    /// This should only be used during token creation when mapping
    /// an object reference to a new opaque scope.
    ///
    /// The scope is deterministic based on object ref + nonce to allow
    /// the same object to have multiple scopes in different tokens.
    pub fn from_object_ref(obj_ref: &ObjectRef, nonce: u64) -> Self {
        use klibcluu::crypto::hash_sha256;

        let mut input = [0u8; 32];

        // Encode object reference
        match obj_ref {
            ObjectRef::Thread(id) => {
                input[0] = 0x01; // Type tag
                input[8..16].copy_from_slice(&id.as_u64().to_le_bytes());
            }
            ObjectRef::Space(id) => {
                input[0] = 0x02;
                input[8..16].copy_from_slice(&id.as_u64().to_le_bytes());
            }
            ObjectRef::Endpoint(id) => {
                input[0] = 0x03;
                input[8..16].copy_from_slice(&id.as_u64().to_le_bytes());
            }
            ObjectRef::Irq(num) => {
                input[0] = 0x04;
                input[8..12].copy_from_slice(&num.to_le_bytes());
            }
            ObjectRef::Reply(id) => {
                input[0] = 0x05;
                input[8..16].copy_from_slice(&id.as_u64().to_le_bytes());
            }
        }

        // Add nonce for uniqueness
        input[16..24].copy_from_slice(&nonce.to_le_bytes());

        // Hash to get opaque scope
        let hash = hash_sha256(&input);
        let mut scope = [0u8; 16];
        scope.copy_from_slice(&hash[..16]);
        Self(scope)
    }
}

impl fmt::Debug for OpaqueScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Scope(")?;
        for (i, byte) in self.0.iter().enumerate() {
            if i > 0 && i % 4 == 0 {
                write!(f, " ")?;
            }
            write!(f, "{:02x}", byte)?;
        }
        write!(f, ")")
    }
}

impl Ord for OpaqueScope {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            match a.cmp(b) {
                core::cmp::Ordering::Equal => continue,
                non_eq => return non_eq,
            }
        }
        core::cmp::Ordering::Equal
    }
}

impl PartialOrd for OpaqueScope {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Object reference (kernel-internal)
///
/// This is what OpaqueScope maps to in the kernel's scope table.
/// Not visible to userspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectRef {
    Thread(crate::sched::ThreadId),
    Space(AddressSpaceId),
    Endpoint(EndpointId),
    Irq(u32),
    /// One-time reply capability for IPC call/reply
    Reply(ReplyId),
}

/// Reply capability identifier (for IPC call/reply)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReplyId(pub u64);

impl ReplyId {
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Address space identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AddressSpaceId(pub u64);

impl AddressSpaceId {
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// IPC endpoint identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EndpointId(pub u64);

impl EndpointId {
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opaque_scope_from_bytes() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let scope = OpaqueScope::from_bytes(bytes);
        assert_eq!(scope.as_bytes(), &bytes);
    }

    #[test]
    fn test_opaque_scope_equality() {
        let bytes1 = [1u8; 16];
        let bytes2 = [1u8; 16];
        let bytes3 = [2u8; 16];

        let scope1 = OpaqueScope::from_bytes(bytes1);
        let scope2 = OpaqueScope::from_bytes(bytes2);
        let scope3 = OpaqueScope::from_bytes(bytes3);

        assert_eq!(scope1, scope2);
        assert_ne!(scope1, scope3);
    }

    #[test]
    fn test_object_ref_variants() {
        use crate::sched::ThreadId;

        let thread_ref = ObjectRef::Thread(ThreadId::new(42));
        let space_ref = ObjectRef::Space(AddressSpaceId::new(17));

        assert!(matches!(thread_ref, ObjectRef::Thread(_)));
        assert!(matches!(space_ref, ObjectRef::Space(_)));
    }
}
