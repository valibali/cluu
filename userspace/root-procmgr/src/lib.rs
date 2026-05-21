//! Process Manager Library
//!
//! This library provides the core functionality for process management,
//! including ELF parsing and loading.

#![cfg_attr(not(feature = "host-test"), no_std)]

extern crate alloc;

pub use elf::{ElfFile, LoadableSegment};
pub use libcluu::elf;

/// Re-export of `cluu_wire` — the wire-protocol types crate.
pub use cluu_wire as proto;

/// Re-exports of modules now living in procmgr-common.
pub use procmgr_common::envelopes;
pub use procmgr_common::manifest_cache;
pub use procmgr_common::mount_policy;
pub use procmgr_common::view_table;

pub mod session_table;
pub mod spawn;
pub mod session_directory;
pub mod dispatch;
pub mod cap_broker;
pub mod restart_root;
pub mod services;
pub mod escalate;
pub mod shutdown;
pub mod proc_query_all;
