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

extern crate alloc;

pub mod allocator;
pub mod boot;
pub mod elf;
pub mod error;
pub mod ipc;
pub mod process;
pub mod rights;
pub mod runtime;
pub mod syscall;
pub mod tar;
pub mod types;

// Re-exports
pub use boot::{
    boot_info, console_info, kbd_info, proc_info, root_token_handle, tty_info, BootInfo,
    CONSOLE_FB_BASE, INITRD_USER_BASE,
};
pub use elf::{ElfFile, LoadableSegment};
pub use error::{Error, Result};
pub use ipc::{call, recv, reply, send};
pub use process::{map_segments, map_stack};
pub use rights::Rights;
pub use syscall::{
    debug_print,

    // High-level wrappers
    endpoint_create,
    ipc_call,
    ipc_recv,
    ipc_recv_timeout,
    ipc_reply,

    // IPC
    ipc_send,
    irq_ack,

    irq_attach,
    space_create,
    space_map,
    thread_create,
    token_derive,
    // Core syscalls
    yield_cpu,
    InvokeOp,
    // Types
    SyscallNumber,
    MAP_DEVICE,
};
pub use types::*;
