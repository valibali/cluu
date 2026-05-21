//! Test-only mock kernel surface. Production code wraps real `libcluu::syscall`;
//! tests inject a recording mock.

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelCall {
    Mint   { parent: u64, rights: u32, new_handle: u64 },
    Revoke { handle: u64 },
    SpawnThread { entry: u64, stack: u64, tid: u64 },
    SendMsg { dest: u64, label: u32, len: usize },
    Recv   { token: u64 },
}

pub trait Kernel {
    fn mint(&mut self, parent: u64, rights: u32) -> u64;
    fn revoke(&mut self, handle: u64);
    fn spawn_thread(&mut self, entry: u64, stack: u64) -> u64;
}

#[derive(Default)]
pub struct MockKernel {
    pub calls: Vec<KernelCall>,
    pub next_handle: u64,
    pub revoked: BTreeMap<u64, bool>,
}

impl MockKernel {
    pub fn new() -> Self {
        Self { calls: Vec::new(), next_handle: 0x1000, revoked: BTreeMap::new() }
    }
}

impl Kernel for MockKernel {
    fn mint(&mut self, parent: u64, rights: u32) -> u64 {
        let new_handle = self.next_handle;
        self.next_handle += 1;
        self.calls.push(KernelCall::Mint { parent, rights, new_handle });
        new_handle
    }
    fn revoke(&mut self, handle: u64) {
        self.calls.push(KernelCall::Revoke { handle });
        self.revoked.insert(handle, true);
    }
    fn spawn_thread(&mut self, entry: u64, stack: u64) -> u64 {
        let tid = self.next_handle;
        self.next_handle += 1;
        self.calls.push(KernelCall::SpawnThread { entry, stack, tid });
        tid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mock_records_mint() {
        let mut k = MockKernel::new();
        let h = k.mint(0xAA, 0xFF);
        assert_eq!(h, 0x1000);
        assert_eq!(k.calls.len(), 1);
    }
    #[test]
    fn mock_records_revoke() {
        let mut k = MockKernel::new();
        k.revoke(0x1000);
        assert!(k.revoked.contains_key(&0x1000));
    }
}
