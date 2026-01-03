//! Kernel support library for CLUU
//!
//! Provides:
//! - Debug output (kprint!, kprintln!)
//! - Kernel utilities
//! - Common helpers
//! - Shared types between kernel modules

#![no_std]
#![cfg_attr(test, allow(unused))]

pub mod debug;
pub mod sync;
pub mod util;

// Re-exports
// Note: kprint! and kprintln! are macros exported at crate root via #[macro_export]
pub use debug::set_debug_output;
pub use sync::SpinLock;
