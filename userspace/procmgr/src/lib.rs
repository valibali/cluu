//! Process Manager Library
//!
//! This library provides the core functionality for process management,
//! including ELF parsing and loading.

#![no_std]

extern crate alloc;

pub use elf::{ElfFile, LoadableSegment};
pub use libcluu::elf;

/// Re-export of `cluu_wire` — the wire-protocol types crate.
pub use cluu_wire as proto;

pub mod manifest_cache;
pub mod view_table;
pub mod session_table;
pub mod spawn;

// Include sub-modules that have `#[cfg(test)]` unit tests so that
// `cargo test -p cluu-procmgr --lib` can discover and run them on the host.
#[cfg(test)]
pub mod envelopes;
