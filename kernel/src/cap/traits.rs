//! Capability Traits
//!
//! This module defines the core traits for the capability system.
//! These traits follow SOLID principles and allow for different
//! implementations and testing strategies.

use crate::cap::{Capability, ObjectRef, Rights};
use crate::error::Error;
use crate::sched::thread::ThreadId;

/// Capability Storage
///
/// Defines the interface for storing and managing capabilities.
/// Implementations typically provide per-process capability tables.
///
/// # Type Parameters
///
/// * `Handle` - Type used to reference stored capabilities (e.g., u8 index)
///
/// # Example
///
/// ```rust,no_run
/// let mut store: impl CapabilityStore<Handle = u8> = ...;
///
/// // Insert capability
/// let handle = store.insert(capability)?;
///
/// // Retrieve capability
/// if let Some(cap) = store.get(handle) {
///     // Use capability
/// }
///
/// // Derive with reduced rights
/// let derived_handle = store.derive(handle, Rights::READ)?;
///
/// // Remove capability
/// store.remove(handle);
/// ```
pub trait CapabilityStore: Send {
    /// Handle type for referencing capabilities
    ///
    /// Typically u8 for capability table indices.
    type Handle: Copy;

    /// Get a capability by handle
    ///
    /// # Arguments
    ///
    /// * `handle` - Handle to capability
    ///
    /// # Returns
    ///
    /// * `Some(&Capability)` - If handle is valid
    /// * `None` - If handle is invalid
    fn get(&self, handle: Self::Handle) -> Option<&Capability>;

    /// Insert a new capability
    ///
    /// Finds an empty slot and stores the capability.
    ///
    /// # Arguments
    ///
    /// * `cap` - Capability to insert
    ///
    /// # Returns
    ///
    /// * `Ok(Handle)` - Handle to inserted capability
    /// * `Err(Error::OutOfMemory)` - If table is full
    fn insert(&mut self, cap: Capability) -> Result<Self::Handle, Error>;

    /// Remove a capability
    ///
    /// Removes capability from storage and invalidates handle.
    ///
    /// # Arguments
    ///
    /// * `handle` - Handle to capability
    ///
    /// # Returns
    ///
    /// * `Some(Capability)` - The removed capability
    /// * `None` - If handle was invalid
    fn remove(&mut self, handle: Self::Handle) -> Option<Capability>;

    /// Derive a new capability with reduced rights
    ///
    /// Creates a new capability to the same object with subset of rights.
    /// This is how rights are delegated.
    ///
    /// # Arguments
    ///
    /// * `handle` - Handle to source capability
    /// * `rights` - Rights for derived capability (must be subset)
    ///
    /// # Returns
    ///
    /// * `Ok(Handle)` - Handle to derived capability
    /// * `Err(Error::NotFound)` - If source handle invalid
    /// * `Err(Error::PermissionDenied)` - If rights not subset
    /// * `Err(Error::OutOfMemory)` - If table is full
    fn derive(&mut self, handle: Self::Handle, rights: Rights)
        -> Result<Self::Handle, Error>;
}

/// Token Validation
///
/// Defines the interface for signing and validating crypto tokens.
/// Tokens are HMAC-based unforgeable capability representations.
///
/// # Security Model
///
/// - Tokens are signed with server-side secret key
/// - HMAC ensures authenticity and integrity
/// - Tokens cannot be forged without key
/// - Epoch field allows batch revocation
///
/// # Example
///
/// ```rust,no_run
/// let validator: impl TokenValidator = ...;
///
/// // Sign a capability into a token
/// let token = validator.sign(&payload);
///
/// // Validate token (anywhere in system)
/// match validator.validate(&token) {
///     Ok(cap) => {
///         // Use capability
///     }
///     Err(_) => {
///         // Invalid or revoked token
///     }
/// }
/// ```
pub trait TokenValidator: Send {
    /// Validate a crypto token
    ///
    /// Verifies HMAC and checks if token is still valid.
    ///
    /// # Arguments
    ///
    /// * `token` - Crypto token to validate
    ///
    /// # Returns
    ///
    /// * `Ok(Capability)` - If token is valid
    /// * `Err(Error::PermissionDenied)` - If HMAC invalid
    /// * `Err(Error::Timeout)` - If token expired (epoch)
    fn validate(&self, token: &[u8; 48]) -> Result<Capability, Error>;

    /// Sign a capability into a crypto token
    ///
    /// Creates an HMAC-signed token from capability data.
    ///
    /// # Arguments
    ///
    /// * `payload` - Capability data to sign
    ///
    /// # Returns
    ///
    /// * Crypto token (HMAC + payload)
    fn sign(&self, payload: &[u8; 16]) -> [u8; 48];
}

/// Access Control
///
/// Defines the interface for checking access control.
/// Determines if a subject has required rights to an object.
///
/// # Example
///
/// ```rust,no_run
/// let ac: impl AccessControl = ...;
///
/// if ac.check(thread_id, object_ref, Rights::READ) {
///     // Access granted
/// } else {
///     // Access denied
/// }
/// ```
pub trait AccessControl: Send {
    /// Check if subject has rights to object
    ///
    /// # Arguments
    ///
    /// * `subject` - Thread requesting access
    /// * `object` - Object being accessed
    /// * `rights` - Required rights
    ///
    /// # Returns
    ///
    /// * `true` - If subject has required rights
    /// * `false` - Otherwise
    fn check(&self, subject: ThreadId, object: ObjectRef, rights: Rights) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mock CapabilityStore for testing
    struct MockStore {
        capabilities: [Option<Capability>; 16],
    }

    impl MockStore {
        fn new() -> Self {
            const NONE: Option<Capability> = None;
            Self {
                capabilities: [NONE; 16],
            }
        }
    }

    impl CapabilityStore for MockStore {
        type Handle = u8;

        fn get(&self, handle: Self::Handle) -> Option<&Capability> {
            self.capabilities.get(handle as usize)?.as_ref()
        }

        fn insert(&mut self, cap: Capability) -> Result<Self::Handle, Error> {
            for (i, slot) in self.capabilities.iter_mut().enumerate() {
                if slot.is_none() {
                    *slot = Some(cap);
                    return Ok(i as u8);
                }
            }
            Err(Error::OutOfMemory)
        }

        fn remove(&mut self, handle: Self::Handle) -> Option<Capability> {
            self.capabilities.get_mut(handle as usize)?.take()
        }

        fn derive(
            &mut self,
            handle: Self::Handle,
            rights: Rights,
        ) -> Result<Self::Handle, Error> {
            let cap = self.get(handle).ok_or(Error::NotFound)?;
            let derived = cap.derive(rights).ok_or(Error::PermissionDenied)?;
            self.insert(derived)
        }
    }

    #[test]
    fn test_mock_store_insert_get() {
        let mut store = MockStore::new();
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(1)), Rights::READ, 0);

        let handle = store.insert(cap).unwrap();
        let retrieved = store.get(handle).unwrap();
        assert_eq!(*retrieved, cap);
    }

    #[test]
    fn test_mock_store_remove() {
        let mut store = MockStore::new();
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(1)), Rights::READ, 0);

        let handle = store.insert(cap).unwrap();
        let removed = store.remove(handle).unwrap();
        assert_eq!(removed, cap);
        assert!(store.get(handle).is_none());
    }

    #[test]
    fn test_mock_store_derive() {
        let mut store = MockStore::new();
        let cap = Capability::new(
            ObjectRef::Thread(ThreadId::new(1)),
            Rights::READ | Rights::WRITE,
            0,
        );

        let handle = store.insert(cap).unwrap();
        let derived_handle = store.derive(handle, Rights::READ).unwrap();

        let derived = store.get(derived_handle).unwrap();
        assert!(derived.has_rights(Rights::READ));
        assert!(!derived.has_rights(Rights::WRITE));
    }

    #[test]
    fn test_mock_store_full() {
        let mut store = MockStore::new();
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(1)), Rights::READ, 0);

        // Fill the store
        for _ in 0..16 {
            store.insert(cap).unwrap();
        }

        // Next insert should fail
        let result = store.insert(cap);
        assert_eq!(result, Err(Error::OutOfMemory));
    }
}
