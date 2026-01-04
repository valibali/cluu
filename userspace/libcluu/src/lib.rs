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
    // Core syscalls
    yield_cpu, debug_print,

    // IPC
    ipc_send, ipc_recv, ipc_call, ipc_reply,

    // High-level wrappers
    thread_create, space_create, space_map, token_derive,
    irq_attach, irq_ack,

    // Types
    SyscallNumber, InvokeOp,
};
pub use types::*;
pub use ipc::{send, recv, call, reply};
