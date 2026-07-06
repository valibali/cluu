//! Capability profiles for process isolation.
//!
//! A CapProfile is a compact bitmask declaring what categories of system
//! interaction a process is allowed to perform. Procmgr translates profiles
//! into concrete kernel token derivations at spawn time.

use bitflags::bitflags;

bitflags! {
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct CapProfile: u16 {
        const IPC         = 1 << 0;
        const SPAWN       = 1 << 1;
        const REGISTRY    = 1 << 2;
        const VFS         = 1 << 3;
        const DEVICE      = 1 << 4;
        const SPACE_GRANT = 1 << 5;
        const NET         = 1 << 6;
        const ADMIN       = 1 << 7;
        const BLOCK_REGION = 1 << 8;
    }
}

impl CapProfile {
    pub const SANDBOXED: Self = Self::IPC;
    pub const USER: Self = Self::IPC
        .union(Self::SPAWN)
        .union(Self::REGISTRY)
        .union(Self::VFS);
    pub const SERVICE: Self = Self::USER.union(Self::DEVICE).union(Self::SPACE_GRANT);
    pub const ADMIN_PROFILE: Self = Self::USER.union(Self::ADMIN);
    pub const SUPERVISOR: Self = Self::SERVICE.union(Self::NET).union(Self::ADMIN);

    /// Check whether `child` is a valid narrowing of `self`.
    pub fn can_grant(self, child: CapProfile) -> bool {
        (child.bits() & !self.bits()) == 0
    }
}
