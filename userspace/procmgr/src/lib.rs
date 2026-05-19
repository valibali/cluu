//! Process Manager Library
//!
//! This library provides the core functionality for process management,
//! including ELF parsing and loading.

#![no_std]

pub use elf::{ElfFile, LoadableSegment};
pub use libcluu::elf;

/// Re-export of `cluu_proto` — the wire-protocol types crate.
pub use cluu_proto as proto;

pub mod manifest_cache;
pub mod view_table;
pub mod spawn;

// Include sub-modules that have `#[cfg(test)]` unit tests so that
// `cargo test -p cluu-procmgr --lib` can discover and run them on the host.
#[cfg(test)]
extern crate alloc;
#[cfg(test)]
pub mod envelopes;
