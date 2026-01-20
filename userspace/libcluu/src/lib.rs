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
pub mod fs;
pub mod ipc;
pub mod mem;
pub mod process;
pub mod registry;
pub mod rights;
pub mod runtime;
pub mod syscall;
pub mod tar;
pub mod types;
pub mod vspace;

// Re-exports
pub use boot::{
    boot_info, process_info, root_token_handle, BootInfo, ProcessInfo, CONSOLE_FB_BASE,
    INITRD_USER_BASE, PARAM_FB_BASE, PARAM_FB_HEIGHT, PARAM_FB_PITCH, PARAM_FB_SIZE,
    PARAM_FB_WIDTH, PARAM_INITRD_SIZE, PROCESS_INFO_ADDR, TOKEN_PROC_CAP, TOKEN_REGISTRY,
    TOKEN_SPACE, TOKEN_STDERR, TOKEN_STDIN, TOKEN_STDLOG, TOKEN_STDOUT,
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
    ipc_recv_any,
    ipc_recv_nonblocking,
    ipc_recv_timeout,
    ipc_reply,

    // IPC
    ipc_send,
    irq_ack,

    irq_attach,
    space_create,
    space_grant,
    space_map,
    space_map_range,
    thread_create,
    token_derive,
    // Core syscalls
    yield_cpu,
    InvokeOp,
    // Types
    SyscallNumber,
    MAP_DEVICE,
    MAP_LARGE_PAGES,
};
pub use types::*;

// Memory constants
pub use mem::{
    is_aligned, is_large_page_aligned, large_page_align_down, large_page_align_up,
    large_pages_for_size, page_align_down, page_align_up, pages_for_size, LARGE_PAGE_SIZE,
    PAGES_PER_LARGE_PAGE, PAGE_SIZE,
};
