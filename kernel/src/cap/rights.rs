//! Capability Rights
//!
//! This module defines the rights that can be granted by capabilities.
//! Rights determine what operations are permitted on a kernel object.
//!
//! # Rights Model
//!
//! Rights follow the principle of least privilege:
//! - Start with no rights (empty)
//! - Explicitly grant each required right
//! - Derive capabilities with subset of rights
//! - Cannot escalate rights (only reduce)
//!
//! # Common Patterns
//!
//! - **READ**: Query object state
//! - **WRITE**: Modify object state
//! - **EXECUTE**: Execute code (for threads/spaces)
//! - **GRANT**: Transfer rights to others
//! - **REVOKE**: Remove rights from others
//!
//! # Example
//!
//! ```rust,no_run
//! // Full access
//! let full = Rights::READ | Rights::WRITE | Rights::EXECUTE;
//!
//! // Read-only
//! let readonly = Rights::READ;
//!
//! // Read-write, no execute
//! let rw = Rights::READ | Rights::WRITE;
//! ```

/// Capability Rights
///
/// Bitflags representing operations that can be performed on objects.
///
/// # Bits
///
/// - Bit 0: READ - Query object state
/// - Bit 1: WRITE - Modify object state
/// - Bit 2: EXECUTE - Execute code (threads/spaces)
/// - Bit 3: GRANT - Transfer capability to others
/// - Bit 4: REVOKE - Revoke capabilities from others
/// - Bit 5: DELETE - Destroy object
/// - Bits 6-31: Reserved for future use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rights(u32);

impl Rights {
    /// No rights
    pub const NONE: Self = Self(0);

    /// Read access - query object state
    ///
    /// Examples:
    /// - Read thread registers
    /// - Read address space mappings
    /// - Receive from IPC endpoint
    pub const READ: Self = Self(1 << 0);

    /// Write access - modify object state
    ///
    /// Examples:
    /// - Modify thread registers
    /// - Map pages in address space
    /// - Send to IPC endpoint
    pub const WRITE: Self = Self(1 << 1);

    /// Execute access - execute code
    ///
    /// Examples:
    /// - Resume thread execution
    /// - Execute code in address space
    pub const EXECUTE: Self = Self(1 << 2);

    /// Grant access - transfer capability to others
    ///
    /// Allows sending this capability via IPC or deriving
    /// new capabilities from it.
    pub const GRANT: Self = Self(1 << 3);

    /// Revoke access - revoke capabilities from others
    ///
    /// Allows revoking derived capabilities or advancing
    /// revocation epochs.
    pub const REVOKE: Self = Self(1 << 4);

    /// Delete access - destroy object
    ///
    /// Allows destroying the object itself.
    pub const DELETE: Self = Self(1 << 5);

    /// Full access - all rights
    pub const FULL: Self = Self(0x3F); // Bits 0-5 set

    /// Create empty rights
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Create rights from raw bits
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Get raw bits
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Check if rights contains specific flags
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Combine rights
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Intersect rights
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Remove rights
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Check if rights is empty
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Check if rights is full (all rights granted)
    pub const fn is_full(self) -> bool {
        (self.0 & Self::FULL.0) == Self::FULL.0
    }
}

// Implement bitwise operators for ergonomic usage
impl core::ops::BitOr for Rights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl core::ops::BitAnd for Rights {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        self.intersection(rhs)
    }
}

impl core::ops::Not for Rights {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

impl core::ops::Sub for Rights {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        self.difference(rhs)
    }
}

impl core::ops::BitOrAssign for Rights {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

impl core::ops::BitAndAssign for Rights {
    fn bitand_assign(&mut self, rhs: Self) {
        *self = self.intersection(rhs);
    }
}

impl core::ops::SubAssign for Rights {
    fn sub_assign(&mut self, rhs: Self) {
        *self = self.difference(rhs);
    }
}

impl Default for Rights {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rights_empty() {
        let rights = Rights::empty();
        assert_eq!(rights, Rights::NONE);
        assert!(rights.is_empty());
        assert!(!rights.contains(Rights::READ));
    }

    #[test]
    fn test_rights_single() {
        let rights = Rights::READ;
        assert!(!rights.is_empty());
        assert!(rights.contains(Rights::READ));
        assert!(!rights.contains(Rights::WRITE));
    }

    #[test]
    fn test_rights_union() {
        let r1 = Rights::READ;
        let r2 = Rights::WRITE;
        let combined = r1 | r2;

        assert!(combined.contains(Rights::READ));
        assert!(combined.contains(Rights::WRITE));
        assert!(!combined.contains(Rights::EXECUTE));
    }

    #[test]
    fn test_rights_intersection() {
        let r1 = Rights::READ | Rights::WRITE;
        let r2 = Rights::WRITE | Rights::EXECUTE;
        let common = r1 & r2;

        assert!(!common.contains(Rights::READ));
        assert!(common.contains(Rights::WRITE));
        assert!(!common.contains(Rights::EXECUTE));
    }

    #[test]
    fn test_rights_difference() {
        let r1 = Rights::READ | Rights::WRITE | Rights::EXECUTE;
        let r2 = Rights::WRITE;
        let diff = r1 - r2;

        assert!(diff.contains(Rights::READ));
        assert!(!diff.contains(Rights::WRITE));
        assert!(diff.contains(Rights::EXECUTE));
    }

    #[test]
    fn test_rights_contains() {
        let rights = Rights::READ | Rights::WRITE;

        assert!(rights.contains(Rights::READ));
        assert!(rights.contains(Rights::WRITE));
        assert!(rights.contains(Rights::READ | Rights::WRITE));
        assert!(!rights.contains(Rights::EXECUTE));
        assert!(!rights.contains(Rights::READ | Rights::EXECUTE));
    }

    #[test]
    fn test_rights_full() {
        let full = Rights::FULL;

        assert!(full.contains(Rights::READ));
        assert!(full.contains(Rights::WRITE));
        assert!(full.contains(Rights::EXECUTE));
        assert!(full.contains(Rights::GRANT));
        assert!(full.contains(Rights::REVOKE));
        assert!(full.contains(Rights::DELETE));
        assert!(full.is_full());
    }

    #[test]
    fn test_rights_assign_operators() {
        let mut rights = Rights::READ;

        rights |= Rights::WRITE;
        assert!(rights.contains(Rights::READ));
        assert!(rights.contains(Rights::WRITE));

        rights &= Rights::WRITE;
        assert!(!rights.contains(Rights::READ));
        assert!(rights.contains(Rights::WRITE));

        rights |= Rights::READ;
        rights -= Rights::WRITE;
        assert!(rights.contains(Rights::READ));
        assert!(!rights.contains(Rights::WRITE));
    }

    #[test]
    fn test_rights_from_bits() {
        let rights = Rights::from_bits(0b111);
        assert!(rights.contains(Rights::READ));
        assert!(rights.contains(Rights::WRITE));
        assert!(rights.contains(Rights::EXECUTE));
    }

    #[test]
    fn test_rights_bits() {
        let rights = Rights::READ | Rights::WRITE;
        assert_eq!(rights.bits(), 0b11);
    }
}
