//! Shared library for root-procmgr and session-procmgr.
//!
//! Contains:
//! - Wire types (`wire`)
//! - IPC label constants (`labels`)
//! - PID encode/decode (`pid`)
//! - Handler dispatch trait (`handler`)
//! - Mock kernel surface for tests (`test_kernel`, `#[cfg(test)]`)
//! - Static envelope/manifest/mount/view utilities ported from legacy procmgr

#![cfg_attr(not(feature = "host-test"), no_std)]

extern crate alloc;

pub mod kernel_iface;
pub mod mint_guard;

pub mod labels;
pub mod pid;
pub mod handler;
pub mod wire;

// test_kernel carries no std dependencies (alloc-only) so it's always included.
// Phase 12 will gate this behind a RealKernel impl when the recv loop is wired.
pub mod test_kernel;

pub mod envelopes;
pub mod manifest_cache;
pub mod mount_policy;
pub mod view_table;
