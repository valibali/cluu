//! Capability System
//!
//! This module implements capability-based security for the CLUU microkernel.
//! Capabilities are unforgeable tokens that grant specific rights to access
//! kernel objects.
//!
//! # Overview
//!
//! CLUU uses capabilities as the sole mechanism for access control:
//! - **Unforgeable**: Capabilities use cryptographic tokens (HMAC)
//! - **Fine-grained**: Rights can be delegated with reduced permissions
//! - **Transferable**: Capabilities can be sent via IPC
//! - **Revocable**: Batch revocation via epoch mechanism
//!
//! # Capability Model
//!
//! A capability consists of:
//! - **Object Reference**: Which kernel object (thread, space, endpoint, etc.)
//! - **Rights**: What operations are permitted (READ, WRITE, GRANT, etc.)
//! - **Epoch**: Revocation epoch for batch invalidation
//!
//! # Crypto Tokens
//!
//! Capabilities can be converted to crypto tokens for transfer:
//! - HMAC-SHA256 based
//! - Self-authenticating (no server-side state needed)
//! - Expire with epoch revocation
//! - Can be validated anywhere in the system
//!
//! # Example
//!
//! ```rust,no_run
//! // Create capability table for a process
//! let mut table = CapabilityTable::new();
//!
//! // Insert capability to thread
//! let cap = Capability::new(
//!     ObjectRef::Thread(ThreadId::new(1)),
//!     Rights::READ | Rights::WRITE,
//!     0, // epoch
//! );
//! let handle = table.insert(cap)?;
//!
//! // Derive capability with reduced rights
//! let read_only = table.derive(handle, Rights::READ)?;
//!
//! // Create crypto token for IPC transfer
//! let token = validator.sign(&cap.into());
//!
//! // Validate token on receiver side
//! let validated_cap = validator.validate(&token)?;
//! ```

pub mod rights;
pub mod traits;
pub mod table;
pub mod token;

// Re-exports
pub use rights::Rights;
pub use traits::{AccessControl, CapabilityStore, TokenValidator};
pub use table::CapabilityTable;
pub use token::{CryptoToken, TokenHandle, TokenPayload};

use crate::sched::thread::ThreadId;

/// Object Reference
///
/// Identifies which kernel object a capability refers to.
///
/// # Kernel Objects
///
/// - **Thread**: Execution context
/// - **Space**: Address space
/// - **Endpoint**: IPC endpoint
/// - **Irq**: Interrupt source
/// - **Null**: Invalid/revoked capability
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectRef {
    /// Null object (invalid/revoked)
    Null,

    /// Thread object
    Thread(ThreadId),

    /// Address space object
    Space(usize),

    /// IPC endpoint object
    Endpoint(usize),

    /// Interrupt source object
    Irq(u8),
}

/// Capability
///
/// Represents the right to perform operations on a kernel object.
///
/// # Structure
///
/// - **object**: Which kernel object
/// - **rights**: What operations are allowed
/// - **epoch**: Revocation epoch (for batch invalidation)
///
/// # Example
///
/// ```rust,no_run
/// let cap = Capability::new(
///     ObjectRef::Thread(ThreadId::new(1)),
///     Rights::READ | Rights::WRITE,
///     0,
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    /// Object this capability refers to
    pub object: ObjectRef,

    /// Rights granted by this capability
    pub rights: Rights,

    /// Revocation epoch
    ///
    /// When the system epoch advances past this value,
    /// the capability is no longer valid.
    pub epoch: u32,
}

impl Capability {
    /// Create a new capability
    ///
    /// # Arguments
    ///
    /// * `object` - Object reference
    /// * `rights` - Rights to grant
    /// * `epoch` - Current revocation epoch
    pub const fn new(object: ObjectRef, rights: Rights, epoch: u32) -> Self {
        Self {
            object,
            rights,
            epoch,
        }
    }

    /// Create a null capability (invalid)
    pub const fn null() -> Self {
        Self {
            object: ObjectRef::Null,
            rights: Rights::empty(),
            epoch: 0,
        }
    }

    /// Check if capability is null/invalid
    pub const fn is_null(&self) -> bool {
        matches!(self.object, ObjectRef::Null)
    }

    /// Check if capability has specific rights
    pub const fn has_rights(&self, rights: Rights) -> bool {
        self.rights.contains(rights)
    }

    /// Derive a new capability with reduced rights
    ///
    /// Creates a new capability to the same object with a subset of rights.
    /// This is how rights are delegated - you can only grant rights you have.
    ///
    /// # Arguments
    ///
    /// * `new_rights` - Rights for derived capability (must be subset)
    ///
    /// # Returns
    ///
    /// * `Some(Capability)` - If new_rights is subset of current rights
    /// * `None` - If trying to grant rights we don't have
    pub fn derive(&self, new_rights: Rights) -> Option<Self> {
        // Can only derive with subset of current rights
        if self.rights.contains(new_rights) {
            Some(Self {
                object: self.object,
                rights: new_rights,
                epoch: self.epoch,
            })
        } else {
            None
        }
    }

    /// Check if capability is still valid for given epoch
    pub const fn is_valid_for_epoch(&self, current_epoch: u32) -> bool {
        self.epoch >= current_epoch && !self.is_null()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_ref_equality() {
        let obj1 = ObjectRef::Thread(ThreadId::new(1));
        let obj2 = ObjectRef::Thread(ThreadId::new(1));
        let obj3 = ObjectRef::Thread(ThreadId::new(2));

        assert_eq!(obj1, obj2);
        assert_ne!(obj1, obj3);
    }

    #[test]
    fn test_capability_new() {
        let cap = Capability::new(
            ObjectRef::Thread(ThreadId::new(1)),
            Rights::READ | Rights::WRITE,
            0,
        );

        assert_eq!(cap.object, ObjectRef::Thread(ThreadId::new(1)));
        assert!(cap.rights.contains(Rights::READ));
        assert!(cap.rights.contains(Rights::WRITE));
        assert_eq!(cap.epoch, 0);
    }

    #[test]
    fn test_capability_null() {
        let cap = Capability::null();
        assert!(cap.is_null());
        assert_eq!(cap.object, ObjectRef::Null);
        assert_eq!(cap.rights, Rights::empty());
    }

    #[test]
    fn test_capability_has_rights() {
        let cap = Capability::new(
            ObjectRef::Thread(ThreadId::new(1)),
            Rights::READ | Rights::WRITE,
            0,
        );

        assert!(cap.has_rights(Rights::READ));
        assert!(cap.has_rights(Rights::WRITE));
        assert!(cap.has_rights(Rights::READ | Rights::WRITE));
        assert!(!cap.has_rights(Rights::EXECUTE));
    }

    #[test]
    fn test_capability_derive() {
        let cap = Capability::new(
            ObjectRef::Thread(ThreadId::new(1)),
            Rights::READ | Rights::WRITE | Rights::EXECUTE,
            0,
        );

        // Derive with subset of rights
        let derived = cap.derive(Rights::READ | Rights::WRITE);
        assert!(derived.is_some());
        let derived = derived.unwrap();
        assert_eq!(derived.object, cap.object);
        assert!(derived.has_rights(Rights::READ));
        assert!(derived.has_rights(Rights::WRITE));
        assert!(!derived.has_rights(Rights::EXECUTE));

        // Cannot derive with rights we don't have
        let invalid = cap.derive(Rights::GRANT);
        assert!(invalid.is_none());
    }

    #[test]
    fn test_capability_epoch_validation() {
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(1)), Rights::READ, 5);

        assert!(cap.is_valid_for_epoch(5));
        assert!(cap.is_valid_for_epoch(4));
        assert!(cap.is_valid_for_epoch(0));
        assert!(!cap.is_valid_for_epoch(6));
        assert!(!cap.is_valid_for_epoch(10));
    }

    #[test]
    fn test_null_capability_invalid() {
        let cap = Capability::null();
        assert!(!cap.is_valid_for_epoch(0));
        assert!(!cap.is_valid_for_epoch(100));
    }
}
