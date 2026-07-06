//! Rights Bitmask - Explicit Permissions
//!
//! Rights are NOT role-based (no "admin", "user" roles).
//! Each bit represents a specific operation allowed.
//!
//! This prevents privilege confusion - you know exactly what operations
//! a token allows by looking at the bitmask.
//!
//! # Design
//!
//! ```text
//! Generic Rights    │ Thread Rights   │ Device Rights   │ Space Rights    │ IPC Rights      │ IRQ Rights
//! ─────────────────┼─────────────────┼─────────────────┼─────────────────┼─────────────────┼────────────────
//! READ     (1<<0)  │ CONTROL  (1<<8) │ IO       (1<<10)│ MAP      (1<<16)│ SEND     (1<<24)│ HANDLE  (1<<28)
//! WRITE    (1<<1)  │ SUSPEND  (1<<9) │ MMIO     (1<<11)│ UNMAP    (1<<17)│ RECV     (1<<25)│ ACK     (1<<29)
//! EXECUTE  (1<<2)  │                 │ DMA      (1<<12)│ GRANT    (1<<18)│ CALL     (1<<26)│ PCI     (1<<30)
//! CREATE   (1<<3)  │                 │ CONFIG   (1<<13)│                 │ REPLY    (1<<27)│
//! DESTROY  (1<<4)  │                 │                 │                 │                 │
//! GRANT    (1<<5)  │                 │                 │                 │                 │
//! MAP      (1<<6)  │                 │                 │                 │                 │
//! MANAGE   (1<<7)  │                 │                 │                 │                 │
//! ```

use core::fmt;

/// Rights bitmask - explicit permissions
///
/// Each bit represents a specific operation allowed.
/// NOT role-based to prevent privilege confusion.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rights(pub u32);

impl Rights {
    // ═══════════════════════════════════════════════════════════════════════
    // Generic Rights (0-7)
    // ═══════════════════════════════════════════════════════════════════════

    /// Read object state
    pub const READ: Self = Self(1 << 0);

    /// Modify object state
    pub const WRITE: Self = Self(1 << 1);

    /// Execute (for code segments)
    pub const EXECUTE: Self = Self(1 << 2);

    /// Create new objects
    pub const CREATE: Self = Self(1 << 3);

    /// Destroy object
    pub const DESTROY: Self = Self(1 << 4);

    /// Create derived tokens (delegate authority)
    pub const GRANT: Self = Self(1 << 5);

    /// Map pages (generic mapping right)
    pub const MAP: Self = Self(1 << 6);

    /// Manage object (modify metadata, configuration)
    pub const MANAGE: Self = Self(1 << 7);

    // ═══════════════════════════════════════════════════════════════════════
    // Thread-Specific Rights (8-15)
    // ═══════════════════════════════════════════════════════════════════════

    /// Modify thread (priority, affinity, etc.)
    pub const THREAD_CONTROL: Self = Self(1 << 8);

    /// Suspend/resume thread
    pub const THREAD_SUSPEND: Self = Self(1 << 9);

    // ═══════════════════════════════════════════════════════════════════════
    // Device-Specific Rights (10-15)
    // ═══════════════════════════════════════════════════════════════════════

    pub const DEVICE_IO: Self = Self(1 << 10);
    pub const DEVICE_MMIO: Self = Self(1 << 11);
    pub const DEVICE_DMA: Self = Self(1 << 12);
    pub const DEVICE_CONFIG: Self = Self(1 << 13);

    // ═══════════════════════════════════════════════════════════════════════
    // Space-Specific Rights (16-23)
    // ═══════════════════════════════════════════════════════════════════════

    /// Map pages into address space
    pub const SPACE_MAP: Self = Self(1 << 16);

    /// Unmap pages from address space
    pub const SPACE_UNMAP: Self = Self(1 << 17);

    /// Grant pages to other address space
    pub const SPACE_GRANT: Self = Self(1 << 18);

    // ═══════════════════════════════════════════════════════════════════════
    // IPC-Specific Rights (24-27)
    // ═══════════════════════════════════════════════════════════════════════

    /// Send to IPC endpoint
    pub const IPC_SEND: Self = Self(1 << 24);

    /// Receive from IPC endpoint
    pub const IPC_RECV: Self = Self(1 << 25);

    /// Call (send + receive) on endpoint
    pub const IPC_CALL: Self = Self(1 << 26);

    /// Reply to an IPC call (one-time reply capability)
    pub const IPC_REPLY: Self = Self(1 << 27);

    // ═══════════════════════════════════════════════════════════════════════
    // IRQ-Specific Rights (28-31)
    // ═══════════════════════════════════════════════════════════════════════

    /// Handle IRQ
    pub const IRQ_HANDLE: Self = Self(1 << 28);

    /// Acknowledge IRQ
    pub const IRQ_ACK: Self = Self(1 << 29);

    /// PCI config space access
    pub const PCI_ACCESS: Self = Self(1 << 30);

    // ═══════════════════════════════════════════════════════════════════════
    // Constructors and Operations
    // ═══════════════════════════════════════════════════════════════════════

    /// No rights
    pub const fn empty() -> Self {
        Self(0)
    }

    /// All rights
    pub const fn all() -> Self {
        Self(u32::MAX)
    }

    /// Create from raw bits
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Get raw bits
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Check if contains a right
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Union of rights
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Intersection of rights
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Check if empty
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Common Right Combinations
    // ═══════════════════════════════════════════════════════════════════════

    /// Read-only rights
    pub const fn read_only() -> Self {
        Self::READ
    }

    /// Read-write rights
    pub const fn read_write() -> Self {
        Self(Self::READ.0 | Self::WRITE.0)
    }

    /// Full thread control
    pub const fn thread_full() -> Self {
        Self(
            Self::READ.0
                | Self::WRITE.0
                | Self::THREAD_CONTROL.0
                | Self::THREAD_SUSPEND.0
                | Self::DESTROY.0
                | Self::GRANT.0,
        )
    }

    /// Full space control
    pub const fn space_full() -> Self {
        Self(
            Self::READ.0
                | Self::SPACE_MAP.0
                | Self::SPACE_UNMAP.0
                | Self::SPACE_GRANT.0
                | Self::DESTROY.0,
        )
    }

    /// Full IPC rights
    pub const fn ipc_full() -> Self {
        Self(Self::IPC_SEND.0 | Self::IPC_RECV.0 | Self::IPC_CALL.0 | Self::IPC_REPLY.0)
    }

    pub const fn device_full() -> Self {
        Self(
            Self::READ.0
                | Self::WRITE.0
                | Self::GRANT.0
                | Self::DEVICE_IO.0
                | Self::DEVICE_MMIO.0
                | Self::DEVICE_DMA.0
                | Self::DEVICE_CONFIG.0,
        )
    }

    /// Serialize to bytes (for signature computation)
    pub const fn to_bytes(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }
}

// Bitwise operators for ergonomic composition
impl core::ops::BitOr for Rights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl core::ops::BitAnd for Rights {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        self.intersection(rhs)
    }
}

impl fmt::Debug for Rights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return write!(f, "Rights(NONE)");
        }

        write!(f, "Rights(")?;
        let mut first = true;

        let mut write_right = |name: &str, right: Rights| {
            if self.contains(right) {
                if !first {
                    write!(f, " | ")?;
                }
                write!(f, "{}", name)?;
                first = false;
            }
            Ok::<(), fmt::Error>(())
        };

        // Generic
        write_right("READ", Self::READ)?;
        write_right("WRITE", Self::WRITE)?;
        write_right("EXECUTE", Self::EXECUTE)?;
        write_right("CREATE", Self::CREATE)?;
        write_right("DESTROY", Self::DESTROY)?;
        write_right("GRANT", Self::GRANT)?;
        write_right("MAP", Self::MAP)?;
        write_right("MANAGE", Self::MANAGE)?;

        // Thread
        write_right("THREAD_CONTROL", Self::THREAD_CONTROL)?;
        write_right("THREAD_SUSPEND", Self::THREAD_SUSPEND)?;

        // Device
        write_right("DEVICE_IO", Self::DEVICE_IO)?;
        write_right("DEVICE_MMIO", Self::DEVICE_MMIO)?;
        write_right("DEVICE_DMA", Self::DEVICE_DMA)?;
        write_right("DEVICE_CONFIG", Self::DEVICE_CONFIG)?;

        // Space
        write_right("SPACE_MAP", Self::SPACE_MAP)?;
        write_right("SPACE_UNMAP", Self::SPACE_UNMAP)?;
        write_right("SPACE_GRANT", Self::SPACE_GRANT)?;

        // IPC
        write_right("IPC_SEND", Self::IPC_SEND)?;
        write_right("IPC_RECV", Self::IPC_RECV)?;
        write_right("IPC_CALL", Self::IPC_CALL)?;
        write_right("IPC_REPLY", Self::IPC_REPLY)?;

        // IRQ
        write_right("IRQ_HANDLE", Self::IRQ_HANDLE)?;
        write_right("IRQ_ACK", Self::IRQ_ACK)?;

        // PCI
        write_right("PCI_ACCESS", Self::PCI_ACCESS)?;

        write!(f, ")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rights_empty() {
        let rights = Rights::empty();
        assert!(rights.is_empty());
        assert_eq!(rights.bits(), 0);
    }

    #[test]
    fn test_rights_contains() {
        let rights = Rights::READ | Rights::WRITE;
        assert!(rights.contains(Rights::READ));
        assert!(rights.contains(Rights::WRITE));
        assert!(!rights.contains(Rights::EXECUTE));
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
    fn test_rights_combinations() {
        let ro = Rights::read_only();
        assert!(ro.contains(Rights::READ));
        assert!(!ro.contains(Rights::WRITE));

        let rw = Rights::read_write();
        assert!(rw.contains(Rights::READ));
        assert!(rw.contains(Rights::WRITE));

        let thread = Rights::thread_full();
        assert!(thread.contains(Rights::THREAD_CONTROL));
        assert!(thread.contains(Rights::THREAD_SUSPEND));

        let space = Rights::space_full();
        assert!(space.contains(Rights::SPACE_MAP));
        assert!(space.contains(Rights::SPACE_UNMAP));

        let ipc = Rights::ipc_full();
        assert!(ipc.contains(Rights::IPC_SEND));
        assert!(ipc.contains(Rights::IPC_RECV));
        assert!(ipc.contains(Rights::IPC_CALL));
    }
}
