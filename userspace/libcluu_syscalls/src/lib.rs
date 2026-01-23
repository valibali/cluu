//! CLUU POSIX Syscall Stubs for C Programs
//!
//! This crate produces a static library (`libcluu_syscalls.a`) that provides
//! the syscall stubs required by newlib. C programs link against this library
//! to get POSIX-compatible system call implementations.
//!
//! # Usage
//!
//! Build this crate as a static library:
//! ```bash
//! cargo build -p libcluu_syscalls --target triplets/x86_64-cluu-user.json
//! ```
//!
//! The resulting `libcluu_syscalls.a` should be installed to the sysroot's
//! lib directory for linking with C programs.

#![no_std]
#![allow(unused_imports)]

extern crate alloc;

// Note: Panic handler is provided by libcluu::runtime for the CLUU target

// Re-export the allocator (required for no_std static library)
pub use libcluu::allocator;

// Note: We intentionally do NOT export libcluu::runtime here because:
// - For C programs, crt0.o provides _start
// - The panic handler in runtime conflicts with crt0
// The allocator initialization is called from __cluu_init() which crt0.S invokes.

// ============================================================================
// POSIX File I/O Stubs
// ============================================================================

pub use libcluu::posix::_close;
pub use libcluu::posix::_lseek;
pub use libcluu::posix::_link;
pub use libcluu::posix::_open;
pub use libcluu::posix::_read;
pub use libcluu::posix::_unlink;
pub use libcluu::posix::_write;

// ============================================================================
// POSIX File Status Stubs
// ============================================================================

pub use libcluu::posix::_fstat;
pub use libcluu::posix::_isatty;
pub use libcluu::posix::_stat;

// ============================================================================
// POSIX Process Stubs
// ============================================================================

pub use libcluu::posix::_execve;
pub use libcluu::posix::_exit;
pub use libcluu::posix::_fork;
pub use libcluu::posix::_getpid;
pub use libcluu::posix::_kill;
pub use libcluu::posix::_wait;
pub use libcluu::posix::posix_spawn;
pub use libcluu::posix::posix_spawnp;
pub use libcluu::posix::system;
pub use libcluu::posix::waitpid;

// ============================================================================
// POSIX Memory Stubs
// ============================================================================

pub use libcluu::posix::_sbrk;
pub use libcluu::posix::brk;

// ============================================================================
// POSIX Time Stubs
// ============================================================================

pub use libcluu::posix::_gettimeofday;
pub use libcluu::posix::_times;
pub use libcluu::posix::clock_gettime;
pub use libcluu::posix::sleep;
pub use libcluu::posix::time;
pub use libcluu::posix::usleep;

// ============================================================================
// Errno Support
// ============================================================================

pub use libcluu::errno::__errno;

// ============================================================================
// C Runtime Initialization
// ============================================================================

pub use libcluu::posix::__cluu_init;

// ============================================================================
// Debug Output
// ============================================================================

pub use libcluu::posix::debug_print;

// ============================================================================
// FD Table (for internal use)
// ============================================================================

pub use libcluu::fd_table::init_stdio;
