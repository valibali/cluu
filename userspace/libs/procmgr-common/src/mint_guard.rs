//! RAII guard that revokes minted caps on drop unless explicitly `forget`-ed.
//! Used in spawn rollback: mint all required caps inside guard, then
//! `mem::forget(guard)` after thread successfully starts.

extern crate alloc;
use alloc::vec::Vec;
use crate::kernel_iface::Kernel;

pub struct MintGuard<'k, K: Kernel> {
    kernel: &'k mut K,
    minted: Vec<u64>,
    armed: bool,
}

impl<'k, K: Kernel> MintGuard<'k, K> {
    pub fn new(kernel: &'k mut K) -> Self {
        Self { kernel, minted: Vec::new(), armed: true }
    }

    /// Mint a new cap derived from `parent` with `rights` and track it for
    /// potential rollback on drop.
    pub fn mint(&mut self, parent: u64, rights: u32) -> u64 {
        let h = self.kernel.mint(parent, rights);
        self.minted.push(h);
        h
    }

    /// Disarm the guard and return all minted handles to the caller.
    /// No revocation happens when the guard is subsequently dropped.
    pub fn forget(mut self) -> Vec<u64> {
        self.armed = false;
        core::mem::take(&mut self.minted)
    }
}

impl<'k, K: Kernel> Drop for MintGuard<'k, K> {
    fn drop(&mut self) {
        if self.armed {
            for h in self.minted.drain(..) {
                self.kernel.revoke(h);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_kernel::{KernelCall, MockKernel};
    use alloc::vec::Vec;

    #[test]
    fn guard_revokes_on_drop_when_armed() {
        let mut k = MockKernel::new();
        {
            let mut g = MintGuard::new(&mut k);
            let _h1 = g.mint(0xAA, 0xFF);
            let _h2 = g.mint(0xBB, 0xFF);
        } // dropped here — both handles should be revoked
        let revokes: Vec<_> = k.calls.iter()
            .filter(|c| matches!(c, KernelCall::Revoke { .. }))
            .collect();
        assert_eq!(revokes.len(), 2, "both minted handles revoked on drop");
    }

    #[test]
    fn forget_disarms_no_revoke() {
        let mut k = MockKernel::new();
        let handles;
        {
            let mut g = MintGuard::new(&mut k);
            g.mint(0xAA, 0xFF);
            g.mint(0xBB, 0xFF);
            handles = g.forget();
        }
        assert_eq!(handles.len(), 2);
        let revokes: Vec<_> = k.calls.iter()
            .filter(|c| matches!(c, KernelCall::Revoke { .. }))
            .collect();
        assert_eq!(revokes.len(), 0, "forget disarms guard — no revocations");
    }
}
