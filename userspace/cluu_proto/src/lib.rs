//! CLUU wire protocol types.
//!
//! This crate is the single source of truth for IPC payload formats
//! shared between libcluu callers and service implementations
//! (procmgr, vfs, compositor, etc.).
//!
//! Specs:
//! - `docs/superpowers/specs/2026-05-18-unified-spawn-protocol-design.md`
//! - `docs/superpowers/specs/2026-05-18-terminal-pty-unification-design.md`
//! - `docs/superpowers/specs/2026-05-18-session-lifecycle-design.md`
//! - `docs/superpowers/specs/2026-05-18-window-protocol-design.md`

#![cfg_attr(not(feature = "host-test"), no_std)]

extern crate alloc;

pub mod spawn;
pub mod primordial;

// Re-exports populated as modules gain content:
pub use spawn::*;
pub use primordial::*;

/// ABI version stamped into `words[1]` of every wire message.
pub const ABI_VERSION: u32 = 1;

/// Caller-side token handle width (matches libcluu/procmgr handle ABI).
pub type TokenHandle = u64;