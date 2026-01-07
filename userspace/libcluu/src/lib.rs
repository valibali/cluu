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

pub mod boot;
pub mod error;
pub mod ipc;
pub mod runtime;
pub mod syscall;
pub mod types;

// Re-exports
pub use boot::{boot_info, root_token_handle, BootInfo};
pub use error::{Error, Result};
pub use ipc::{call, recv, reply, send};
pub use syscall::{
    debug_print,

    ipc_call,
    ipc_recv,
    ipc_reply,

    // IPC
    ipc_send,
    irq_ack,

    irq_attach,
    space_create,
    space_map,
    // High-level wrappers
    thread_create,
    token_derive,
    // Core syscalls
    yield_cpu,
    InvokeOp,
    // Types
    SyscallNumber,
};
pub use types::*;
