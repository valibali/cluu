//! CLUU Userspace Library
//!
//! Provides syscall wrappers, IPC helpers, and runtime support for userspace programs
//!
//! # Usage
//!
//! ```no_run
//! use libcluu::{debug_print, yield_cpu, Result};
//!
//! fn main() -> Result<()> {
//!     debug_print("Hello from userspace!")?;
//!     yield_cpu()?;
//!     Ok(())
//! }
//! ```

#![no_std]
#![cfg_attr(all(not(feature = "std"), not(test)), no_main)]

pub mod error;
pub mod syscall;
pub mod ipc;
pub mod types;
pub mod runtime;

// Re-exports
pub use error::{Error, Result};
pub use syscall::{
    yield_cpu, debug_print, token_create, token_delete,
    irq_attach, irq_ack, CapabilityToken,
};
pub use types::*;
pub use ipc::{send, recv, call, reply};
