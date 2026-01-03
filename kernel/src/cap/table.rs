//! Capability Table
//!
//! This module implements per-process capability tables for storing
//! and managing capabilities.
//!
//! # Design
//!
//! - Fixed-size array of 256 capability slots
//! - Index-based handles (u8)
//! - O(1) lookup, insert, remove
//! - Sparse storage (Option<Capability>)
//!
//! # Example
//!
//! ```rust,no_run
//! let mut table = CapabilityTable::new();
//!
//! // Insert capability
//! let handle = table.insert(capability)?;
//!
//! // Use capability
//! if let Some(cap) = table.get(handle) {
//!     if cap.has_rights(Rights::READ) {
//!         // Perform read operation
//!     }
//! }
//!
//! // Derive with reduced rights
//! let readonly = table.derive(handle, Rights::READ)?;
//!
//! // Remove capability
//! table.remove(handle);
//! ```

use crate::cap::traits::CapabilityStore;
use crate::cap::{Capability, Rights};
use crate::error::Error;

/// Capability Table
///
/// Per-process storage for capabilities.
///
/// # Capacity
///
/// - 256 capability slots (indexed 0-255)
/// - Sparse storage (Option<Capability>)
/// - First-fit allocation for new capabilities
///
/// # Performance
///
/// - get(): O(1)
/// - insert(): O(n) worst case (linear search for empty slot)
/// - remove(): O(1)
/// - derive(): O(n) worst case (insert cost)
pub struct CapabilityTable {
    /// Capability slots (256 max)
    slots: [Option<Capability>; 256],

    /// Number of capabilities currently stored
    count: usize,
}

impl CapabilityTable {
    /// Create a new empty capability table
    pub fn new() -> Self {
        const NONE: Option<Capability> = None;
        Self {
            slots: [NONE; 256],
            count: 0,
        }
    }

    /// Get number of capabilities in table
    pub fn len(&self) -> usize {
        self.count
    }

    /// Check if table is empty
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Check if table is full
    pub fn is_full(&self) -> bool {
        self.count >= 256
    }

    /// Get mutable reference to capability
    ///
    /// For internal use (e.g., updating revocation epochs).
    pub fn get_mut(&mut self, handle: u8) -> Option<&mut Capability> {
        self.slots[handle as usize].as_mut()
    }

    /// Find first empty slot
    ///
    /// Returns slot index if found, or None if table is full.
    fn find_empty_slot(&self) -> Option<u8> {
        self.slots
            .iter()
            .position(|slot| slot.is_none())
            .map(|i| i as u8)
    }

    /// Clear all capabilities from table
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.count = 0;
    }

    /// Revoke all capabilities for a specific object
    ///
    /// Removes all capabilities that refer to the given object.
    /// Returns the number of capabilities revoked.
    pub fn revoke_object(&mut self, object: crate::cap::ObjectRef) -> usize {
        let mut revoked = 0;
        for slot in &mut self.slots {
            if let Some(cap) = slot {
                if cap.object == object {
                    *slot = None;
                    revoked += 1;
                    self.count -= 1;
                }
            }
        }
        revoked
    }

    /// Update revocation epoch for all capabilities
    ///
    /// Invalidates capabilities with epoch older than new_epoch.
    /// Returns the number of capabilities invalidated.
    pub fn advance_epoch(&mut self, new_epoch: u32) -> usize {
        let mut invalidated = 0;
        for slot in &mut self.slots {
            if let Some(cap) = slot {
                if cap.epoch < new_epoch {
                    *slot = None;
                    invalidated += 1;
                    self.count -= 1;
                }
            }
        }
        invalidated
    }

    /// Iterate over all capabilities
    pub fn iter(&self) -> impl Iterator<Item = (u8, &Capability)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|cap| (i as u8, cap)))
    }
}

impl CapabilityStore for CapabilityTable {
    type Handle = u8;

    fn get(&self, handle: Self::Handle) -> Option<&Capability> {
        self.slots[handle as usize].as_ref()
    }

    fn insert(&mut self, cap: Capability) -> Result<Self::Handle, Error> {
        let slot_idx = self.find_empty_slot().ok_or(Error::OutOfMemory)?;
        self.slots[slot_idx as usize] = Some(cap);
        self.count += 1;
        Ok(slot_idx)
    }

    fn remove(&mut self, handle: Self::Handle) -> Option<Capability> {
        let cap = self.slots[handle as usize].take();
        if cap.is_some() {
            self.count -= 1;
        }
        cap
    }

    fn derive(&mut self, handle: Self::Handle, rights: Rights) -> Result<Self::Handle, Error> {
        // Get source capability
        let source = self.get(handle).ok_or(Error::NotFound)?;

        // Derive new capability with reduced rights
        let derived = source.derive(rights).ok_or(Error::PermissionDenied)?;

        // Insert derived capability
        self.insert(derived)
    }
}

impl Default for CapabilityTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cap::ObjectRef;
    use crate::sched::thread::ThreadId;
    use alloc::vec::Vec;

    #[test]
    fn test_table_new() {
        let table = CapabilityTable::new();
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
        assert!(!table.is_full());
    }

    #[test]
    fn test_table_insert_get() {
        let mut table = CapabilityTable::new();
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(1)), Rights::READ, 0);

        let handle = table.insert(cap).unwrap();
        assert_eq!(table.len(), 1);

        let retrieved = table.get(handle).unwrap();
        assert_eq!(*retrieved, cap);
    }

    #[test]
    fn test_table_remove() {
        let mut table = CapabilityTable::new();
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(1)), Rights::READ, 0);

        let handle = table.insert(cap).unwrap();
        assert_eq!(table.len(), 1);

        let removed = table.remove(handle).unwrap();
        assert_eq!(removed, cap);
        assert_eq!(table.len(), 0);
        assert!(table.get(handle).is_none());
    }

    #[test]
    fn test_table_derive() {
        let mut table = CapabilityTable::new();
        let cap = Capability::new(
            ObjectRef::Thread(ThreadId::new(1)),
            Rights::READ | Rights::WRITE | Rights::EXECUTE,
            0,
        );

        let handle = table.insert(cap).unwrap();
        let derived_handle = table.derive(handle, Rights::READ | Rights::WRITE).unwrap();

        let derived = table.get(derived_handle).unwrap();
        assert!(derived.has_rights(Rights::READ));
        assert!(derived.has_rights(Rights::WRITE));
        assert!(!derived.has_rights(Rights::EXECUTE));
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn test_table_derive_invalid_rights() {
        let mut table = CapabilityTable::new();
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(1)), Rights::READ, 0);

        let handle = table.insert(cap).unwrap();

        // Try to derive with rights we don't have
        let result = table.derive(handle, Rights::WRITE);
        assert_eq!(result, Err(Error::PermissionDenied));
    }

    #[test]
    fn test_table_full() {
        let mut table = CapabilityTable::new();
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(1)), Rights::READ, 0);

        // Fill table
        for _ in 0..256 {
            table.insert(cap).unwrap();
        }

        assert!(table.is_full());
        assert_eq!(table.len(), 256);

        // Next insert should fail
        let result = table.insert(cap);
        assert_eq!(result, Err(Error::OutOfMemory));
    }

    #[test]
    fn test_table_clear() {
        let mut table = CapabilityTable::new();
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(1)), Rights::READ, 0);

        for _ in 0..10 {
            table.insert(cap).unwrap();
        }
        assert_eq!(table.len(), 10);

        table.clear();
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
    }

    #[test]
    fn test_table_revoke_object() {
        let mut table = CapabilityTable::new();
        let obj1 = ObjectRef::Thread(ThreadId::new(1));
        let obj2 = ObjectRef::Thread(ThreadId::new(2));

        table.insert(Capability::new(obj1, Rights::READ, 0)).unwrap();
        table.insert(Capability::new(obj2, Rights::READ, 0)).unwrap();
        table.insert(Capability::new(obj1, Rights::WRITE, 0)).unwrap();
        assert_eq!(table.len(), 3);

        // Revoke all capabilities for obj1
        let revoked = table.revoke_object(obj1);
        assert_eq!(revoked, 2);
        assert_eq!(table.len(), 1);

        // Only obj2 capability should remain
        let remaining: Vec<_> = table.iter().collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].1.object, obj2);
    }

    #[test]
    fn test_table_advance_epoch() {
        let mut table = CapabilityTable::new();

        table.insert(Capability::new(
            ObjectRef::Thread(ThreadId::new(1)),
            Rights::READ,
            0,
        )).unwrap();
        table.insert(Capability::new(
            ObjectRef::Thread(ThreadId::new(2)),
            Rights::READ,
            5,
        )).unwrap();
        table.insert(Capability::new(
            ObjectRef::Thread(ThreadId::new(3)),
            Rights::READ,
            10,
        )).unwrap();
        assert_eq!(table.len(), 3);

        // Advance epoch to 6 - should invalidate caps with epoch < 6
        let invalidated = table.advance_epoch(6);
        assert_eq!(invalidated, 2); // epochs 0 and 5 invalidated
        assert_eq!(table.len(), 1);

        // Only epoch 10 capability should remain
        let remaining: Vec<_> = table.iter().collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].1.epoch, 10);
    }

    #[test]
    fn test_table_iter() {
        let mut table = CapabilityTable::new();

        let h1 = table.insert(Capability::new(
            ObjectRef::Thread(ThreadId::new(1)),
            Rights::READ,
            0,
        )).unwrap();
        let h2 = table.insert(Capability::new(
            ObjectRef::Thread(ThreadId::new(2)),
            Rights::WRITE,
            0,
        )).unwrap();
        let h3 = table.insert(Capability::new(
            ObjectRef::Thread(ThreadId::new(3)),
            Rights::EXECUTE,
            0,
        )).unwrap();

        let caps: Vec<_> = table.iter().collect();
        assert_eq!(caps.len(), 3);

        // Check that all handles are present
        let handles: Vec<u8> = caps.iter().map(|(h, _)| *h).collect();
        assert!(handles.contains(&h1));
        assert!(handles.contains(&h2));
        assert!(handles.contains(&h3));
    }

    #[test]
    fn test_table_get_mut() {
        let mut table = CapabilityTable::new();
        let cap = Capability::new(ObjectRef::Thread(ThreadId::new(1)), Rights::READ, 5);

        let handle = table.insert(cap).unwrap();

        // Update epoch using get_mut
        if let Some(cap) = table.get_mut(handle) {
            cap.epoch = 10;
        }

        let retrieved = table.get(handle).unwrap();
        assert_eq!(retrieved.epoch, 10);
    }
}
