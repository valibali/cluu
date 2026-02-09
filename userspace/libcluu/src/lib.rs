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
pub mod device_io;
pub mod elf;
pub mod error;
pub mod fs;
pub mod ipc;
pub mod mem;
pub mod pci;
pub mod process;
pub mod registry;
pub mod rights;
pub mod runtime;
pub mod syscall;
pub mod tar;
pub mod time;
pub mod types;
pub mod vspace;

// POSIX syscall stubs (feature-gated for newlib compatibility)
#[cfg(feature = "posix")]
pub mod errno;
#[cfg(feature = "posix")]
pub mod fd_table;
#[cfg(feature = "posix")]
pub mod posix;

// Re-exports
pub use boot::{
    // Structs and functions
    boot_info, process_info, root_token_handle, BootInfo, ProcessInfo,
    // Convenience accessors
    pid, space_token, stderr, stdin, stdout, token, token_clock, token_extra, token_ipc,
    token_registry, token_self,
    // Address constants
    CONSOLE_FB_BASE, INITRD_USER_BASE, PROCESS_INFO_ADDR,
    // Param indices
    PARAM_FB_BASE, PARAM_FB_HEIGHT, PARAM_FB_PITCH, PARAM_FB_SIZE, PARAM_FB_WIDTH,
    PARAM_INITRD_SIZE,
    // Token slot constants - Universal (0-8)
    TOKEN_STDIN, TOKEN_STDOUT, TOKEN_STDERR, TOKEN_STDLOG, // I/O (0-3)
    TOKEN_SELF, TOKEN_SPACE, TOKEN_IPC, TOKEN_CLOCK,       // Core caps (4-7)
    TOKEN_REGISTRY,                                         // System (8)
    // Token slot constants - Contextual (9-15)
    TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2, TOKEN_EXTRA_3, TOKEN_EXTRA_4, TOKEN_EXTRA_5,
    TOKEN_EXTRA_6,
    // Slot ranges
    TOKEN_CAPS_END, TOKEN_CAPS_START, TOKEN_EXTRA_END, TOKEN_EXTRA_START, TOKEN_STDIO_END,
    TOKEN_STDIO_START,
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
    clock_frequency,
    clock_now,
    space_create,
    space_grant,
    space_map,
    space_map_range,
    space_unmap,
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
