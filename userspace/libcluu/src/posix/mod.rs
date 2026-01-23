//! POSIX syscall stubs for newlib compatibility.
//!
//! This module provides C-compatible syscall stubs that bridge POSIX semantics
//! to CLUU's IPC-based architecture. These are used by newlib to implement
//! standard C library functions.
//!
//! # Supported Functions
//!
//! - File I/O: `_open`, `_close`, `_read`, `_write`, `_lseek`
//! - File status: `_fstat`, `_stat`, `_isatty`
//! - Process: `_exit`, `_getpid`, `_kill`, `_wait`, `posix_spawn`
//! - Memory: `_sbrk`
//! - Stubs: `_fork`, `_execve`, `_link`, `_unlink`
//!
//! # Usage
//!
//! Enable the `posix` feature in Cargo.toml:
//! ```toml
//! [dependencies]
//! libcluu = { path = "...", features = ["posix"] }
//! ```

// Allow non-camel-case types for C compatibility
#![allow(non_camel_case_types)]

mod file;
mod memory;
mod process;
mod stat;
mod time;

pub use file::*;
pub use memory::*;
pub use process::*;
pub use stat::*;
pub use time::*;

// C type aliases
pub type c_int = i32;
pub type c_uint = u32;
pub type c_char = i8;
pub type c_void = core::ffi::c_void;
pub type c_long = i64;
pub type c_ulong = u64;
pub type size_t = usize;
pub type ssize_t = isize;
pub type off_t = i64;
pub type pid_t = i32;
pub type mode_t = u32;
pub type clock_t = i64;
pub type time_t = i64;

/// Initialize POSIX layer.
///
/// Call this from `__cluu_init()` to set up fd table with stdio.
pub fn init() {
    crate::fd_table::init_stdio();
}

/// C-callable debug print (outputs to kernel log).
///
/// Useful for debugging C programs before stdout is working.
#[no_mangle]
pub extern "C" fn debug_print(msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let s = unsafe {
        let mut len = 0;
        let mut p = msg;
        while *p != 0 {
            len += 1;
            p = p.add(1);
        }
        core::str::from_utf8_unchecked(core::slice::from_raw_parts(msg as *const u8, len))
    };
    let _ = crate::syscall::debug_print(s);
}

/// C-callable runtime initialization.
///
/// This is called by crt0.S before main() to initialize:
/// - Heap allocator
/// - File descriptor table (fd 0-3 = stdin/stdout/stderr/stdlog)
///
/// For Rust programs, this is handled by `_start` in runtime.rs.
/// For C programs using crt0.S, this function must be called.
#[no_mangle]
pub extern "C" fn __cluu_init() {
    crate::allocator::init();
    crate::fd_table::init_stdio();
}
