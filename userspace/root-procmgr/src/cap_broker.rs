//! Cap broker: root-procmgr mints session-scoped caps from its primordial
//! handles. Each mint narrows rights (monotone).

extern crate alloc;
use procmgr_common::mint_guard::MintGuard;
use procmgr_common::kernel_iface::Kernel;

/// Rights bitmask for a capability.
#[derive(Debug, Clone, Copy)]
pub struct CapRights(pub u32);

/// Mint a child cap from `parent` with `requested` rights, enforcing that
/// `requested` is a strict subset of `parent_rights` (monotone narrowing).
pub fn sub_mint<K: Kernel>(
    guard: &mut MintGuard<'_, K>,
    parent: u64,
    parent_rights: CapRights,
    requested: CapRights,
) -> Result<u64, BrokerError> {
    if requested.0 & !parent_rights.0 != 0 {
        return Err(BrokerError::WiderThanParent);
    }
    Ok(guard.mint(parent, requested.0))
}

#[derive(Debug, PartialEq, Eq)]
pub enum BrokerError { WiderThanParent }

// ─── Constants ───────────────────────────────────────────────────────────────

/// Rights granted to session-procmgr for the VFS cap.
pub const VFS_SESSION_RIGHTS: u32 = 0x07;
/// Rights granted to session-procmgr for the registry cap.
pub const REGISTRY_SESSION_RIGHTS: u32 = 0x03;
/// Rights granted to session-procmgr for the timeserver cap.
pub const TIMESERVER_SESSION_RIGHTS: u32 = 0x01;

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::test_kernel::MockKernel;
    use proptest::prelude::*;

    #[test]
    fn narrowing_ok() {
        let mut k = MockKernel::new();
        let mut g = MintGuard::new(&mut k);
        let result = sub_mint(&mut g, 0xAA00, CapRights(0xFF), CapRights(0x0F));
        assert!(result.is_ok(), "narrowing from 0xFF to 0x0F should succeed");
    }

    #[test]
    fn widening_fails() {
        let mut k = MockKernel::new();
        let mut g = MintGuard::new(&mut k);
        let result = sub_mint(&mut g, 0xAA00, CapRights(0x0F), CapRights(0xFF));
        assert_eq!(result, Err(BrokerError::WiderThanParent));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]
        #[test]
        fn prop_child_subset_of_parent(parent_rights: u32, req_rights: u32) {
            let mut k = MockKernel::new();
            let mut g = MintGuard::new(&mut k);
            let result = sub_mint(&mut g, 0xDEAD, CapRights(parent_rights), CapRights(req_rights));
            if req_rights & !parent_rights == 0 {
                prop_assert!(result.is_ok(), "subset should succeed");
            } else {
                prop_assert_eq!(result, Err(BrokerError::WiderThanParent));
            }
        }
    }
}
