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

pub mod labels;
pub mod pid;
pub mod handler;
pub mod wire;

#[cfg(any(test, feature = "host-test"))]
pub mod test_kernel;

pub mod envelopes;
pub mod manifest_cache;
pub mod mount_policy;
pub mod view_table;
