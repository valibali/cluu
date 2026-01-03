//! Kernel support library for CLUU
//!
//! Provides:
//! - Debug output (kprint!, kprintln!)
//! - Kernel utilities
//! - Common helpers
//! - Shared types between kernel modules

#![no_std]
#![cfg_attr(test, allow(unused))]

pub mod logger;
pub mod sync;
pub mod uart;
pub mod util;

// Re-exports
pub use sync::SpinLock;

// IRQ-safe UART driver (COM2 at 0x2F8)
pub use uart::{Uart, COM2};

// IRQ-safe logger (zero-cost in release builds)
pub use logger::{LogLevel, init as logger_init, log, error, warn, info, debug, trace, log_hex, log_dec, should_log};
