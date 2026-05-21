extern crate alloc;
use procmgr_common::kernel_iface::Kernel;
use procmgr_common::mint_guard::MintGuard;

/// Capability rights bitmask.
#[derive(Debug, Clone, Copy)]
pub struct CapRights(pub u32);

#[derive(Debug, PartialEq, Eq)]
pub enum BrokerError {
    WiderThanParent,
}

/// Mint a capability with rights that are a sub-set of the parent's rights.
/// Returns `Err(WiderThanParent)` if `requested` has bits not present in `parent_rights`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::mint_guard::MintGuard;
    use procmgr_common::test_kernel::MockKernel;

    #[test]
    fn sub_mint_within_rights_succeeds() {
        let mut k = MockKernel::new();
        let mut guard = MintGuard::new(&mut k);
        let h = sub_mint(&mut guard, 0xAA, CapRights(0x07), CapRights(0x03)).unwrap();
        assert_ne!(h, 0);
    }

    #[test]
    fn sub_mint_wider_than_parent_rejected() {
        let mut k = MockKernel::new();
        let mut guard = MintGuard::new(&mut k);
        let err = sub_mint(&mut guard, 0xAA, CapRights(0x01), CapRights(0x03)).unwrap_err();
        assert_eq!(err, BrokerError::WiderThanParent);
    }
}
