//! Abstract kernel surface used by `MintGuard` and production wrappers.
//!
//! The `Kernel` trait is always compiled (not test-gated) so that `MintGuard`
//! can reference it in production code.  The concrete `MockKernel` lives in
//! `test_kernel` which remains gated on `#[cfg(any(test, feature = "host-test"))]`.

pub trait Kernel {
    /// Mint a new capability derived from `parent` with the given rights mask.
    /// Returns a new handle that the caller owns.
    fn mint(&mut self, parent: u64, rights: u32) -> u64;

    /// Revoke and destroy a capability handle.
    fn revoke(&mut self, handle: u64);

    /// Spawn a new thread at `entry` with stack pointer `stack`.
    /// Returns the new thread's TID handle.
    fn spawn_thread(&mut self, entry: u64, stack: u64) -> u64;

    /// Destroy a thread by its thread token. Errors are silently ignored.
    fn thread_destroy(&mut self, thread_tok: u64);

    /// Destroy an address space by its space token. Errors are silently ignored.
    fn space_destroy(&mut self, space_tok: u64);
}
