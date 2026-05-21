//! Production wiring of the `Kernel` trait to actual `libcluu` syscalls.
//!
//! This module is excluded from host-test builds because the underlying
//! `libcluu::syscall` functions emit real x86-64 `syscall` instructions
//! and cannot execute on the host.  Tests inject `MockKernel` instead.

use procmgr_common::kernel_iface::Kernel;

/// Live `Kernel` implementation backed by `libcluu` syscall wrappers.
pub struct RealKernel;

impl Kernel for RealKernel {
    /// Derive a new capability from `parent` with the given rights mask.
    ///
    /// Maps to `token_derive(parent, rights, expire_at=0)`.  Returns `0`
    /// on failure (invalid parent or rights narrowing rejected).
    fn mint(&mut self, parent: u64, rights: u32) -> u64 {
        libcluu::syscall::token_derive(parent as usize, rights as usize, 0)
            .unwrap_or(0) as u64
    }

    /// Revoke and destroy a capability handle.
    ///
    /// Maps to `token_revoke(handle)`.  Errors are silently dropped; a
    /// double-revoke is benign from the caller's perspective.
    fn revoke(&mut self, handle: u64) {
        let _ = libcluu::syscall::token_revoke(handle as usize);
    }

    /// Spawn a new thread at `entry` with stack pointer `stack`.
    ///
    /// Uses the process's own address-space token (`TOKEN_SPACE`) so the
    /// thread runs inside the same address space.  Returns `0` on failure.
    fn spawn_thread(&mut self, entry: u64, stack: u64) -> u64 {
        libcluu::syscall::thread_create(
            libcluu::boot::space_token(),
            entry as usize,
            stack as usize,
            0, // default priority
            0, // flags: start running immediately
        )
        .unwrap_or(0) as u64
    }
}
