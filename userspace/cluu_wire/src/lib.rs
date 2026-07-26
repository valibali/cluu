//! CLUU wire protocol types.
//!
//! This crate is the single source of truth for IPC payload formats
//! shared between libcluu callers and service implementations
//! (procmgr, vfs, compositor, etc.).
//!
//! Specs (extracted into book chapters):
//! - `doc/book/procmgr.md` (unified spawn protocol)
//! - `doc/book/terminal.md` (terminal-pty unification, window protocol)
//! - `doc/book/sessions.md` (session lifecycle)

#![cfg_attr(not(feature = "host-test"), no_std)]

extern crate alloc;

pub mod spawn;
pub mod primordial;
pub mod pts;
pub mod session;
pub mod display;

// Re-exports populated as modules gain content:
pub use spawn::*;
pub use primordial::*;

/// ABI version stamped into `words[1]` of every wire message.
pub const ABI_VERSION: u32 = 1;

/// Caller-side token handle width (matches libcluu/procmgr handle ABI).
pub type TokenHandle = u64;