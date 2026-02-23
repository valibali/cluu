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
// C-ABI entry points in POSIX/newlib shims intentionally validate/deref raw pointers
// inside safe `extern "C"` wrappers to preserve libc-style call sites.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

extern crate alloc;

pub mod allocator;
pub mod boot;
pub mod boot_manifest;
pub mod cap;
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
pub mod toml;
pub mod types;
pub mod vfs_view;
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
    boot_info,
    // Convenience accessors
    pid,
    process_info,
    root_token_handle,
    space_token,
    stderr,
    stdin,
    stdout,
    token,
    token_clock,
    token_extra,
    token_ipc,
    token_registry,
    token_self,
    BootInfo,
    ProcessInfo,
    // Address constants
    CONSOLE_FB_BASE,
    INITRD_USER_BASE,
    // Param indices
    PARAM_FB_BASE,
    PARAM_FB_HEIGHT,
    PARAM_FB_PITCH,
    PARAM_FB_SIZE,
    PARAM_FB_WIDTH,
    PARAM_INITRD_SIZE,
    PARAM_VFS_FB_PHYS,
    PARAM_VFS_FB_SIZE,
    PARAM_VFS_FB_WIDTH,
    PARAM_VFS_FB_HEIGHT,
    PARAM_VFS_FB_PITCH,
    PROCESS_INFO_ADDR,
    // Slot ranges
    TOKEN_CAPS_END,
    TOKEN_CAPS_START,
    TOKEN_CLOCK, // Core caps (4-7)
    // Token slot constants - Contextual (9-15)
    TOKEN_EXTRA_0,
    TOKEN_EXTRA_1,
    TOKEN_EXTRA_2,
    TOKEN_EXTRA_3,
    TOKEN_EXTRA_4,
    TOKEN_EXTRA_5,
    TOKEN_EXTRA_6,
    TOKEN_EXTRA_END,
    TOKEN_EXTRA_START,
    TOKEN_IPC,
    TOKEN_REGISTRY, // System (8)
    TOKEN_SELF,
    TOKEN_SPACE,
    TOKEN_STDERR,
    // Token slot constants - Universal (0-8)
    TOKEN_STDIN,
    TOKEN_STDIO_END,
    TOKEN_STDIO_START,
    TOKEN_STDLOG, // I/O (0-3)
    TOKEN_STDOUT,
};
pub use elf::{ElfFile, LoadableSegment};
pub use error::{Error, Result};
pub use ipc::{call, recv, reply, send};
pub use process::{map_segments, map_stack};
pub use rights::Rights;
pub use syscall::{
    clock_frequency,
    clock_now,
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
    space_protect,
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
