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

mod dir;
pub mod env;
mod fcntl;
pub mod file;
pub mod framebuffer;
mod memory;
pub mod jobs;
pub mod pipe;
mod poll;
mod process;
pub mod pthread;
pub mod signal;
pub mod socket;
mod stat;
mod stubs;
pub mod termios;
mod time;
pub mod tty;

pub use dir::*;
pub use env::*;
pub use file::*;
pub use memory::*;
pub use process::*;
pub use pthread::*;
pub use stat::*;
pub use termios::*;
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
    init_tls();
    crate::fd_table::init_stdio();
    let _ = crate::registry::init("app");
    dir::init_cwd();
    env::init_env();
}

/// Set up TLS block for the main thread.
///
/// Uses the shared `pthread::alloc_tls_block()` which copies .tdata, zeroes
/// .tbss, and writes the TCB self-pointer. Then sets FS base via
/// ThreadSetFSBase so `__thread` / `_Thread_local` variables work.
pub fn init_tls() {
    let (_block_addr, tcb_addr) = match pthread::alloc_tls_block() {
        Some(t) => t,
        None => return,
    };

    let space_token = crate::boot::space_token();
    let _ = crate::syscall::thread_set_fs_base(space_token, tcb_addr);

    let self_token = crate::boot::token_self();
    unsafe {
        core::ptr::write((tcb_addr + 8) as *mut usize, self_token);
    }
}

// -------------------------------------------------------------------------
// newlib compatibility shims (non-underscore + reentrant forms)
// -------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn open(path: *const c_char, flags: c_int, mode: mode_t) -> c_int {
    _open(path, flags, mode)
}

#[no_mangle]
pub extern "C" fn mkdir(path: *const c_char, mode: mode_t) -> c_int {
    _mkdir(path, mode)
}

#[no_mangle]
pub extern "C" fn rmdir(path: *const c_char) -> c_int {
    _rmdir(path)
}

#[no_mangle]
pub extern "C" fn rename(old: *const c_char, new: *const c_char) -> c_int {
    _rename(old, new)
}

#[no_mangle]
pub extern "C" fn unlink(path: *const c_char) -> c_int {
    _unlink(path)
}

#[no_mangle]
pub extern "C" fn close(fd: c_int) -> c_int {
    _close(fd)
}

#[no_mangle]
pub extern "C" fn dup(fd: c_int) -> c_int {
    _dup(fd)
}

#[no_mangle]
pub extern "C" fn dup2(oldfd: c_int, newfd: c_int) -> c_int {
    _dup2(oldfd, newfd)
}

#[no_mangle]
pub extern "C" fn read(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
    _read(fd, buf, count)
}

#[no_mangle]
pub extern "C" fn write(fd: c_int, buf: *const c_void, count: size_t) -> ssize_t {
    _write(fd, buf, count)
}

#[no_mangle]
pub extern "C" fn lseek(fd: c_int, offset: off_t, whence: c_int) -> off_t {
    _lseek(fd, offset, whence)
}

#[no_mangle]
pub extern "C" fn fstat(fd: c_int, st: *mut stat::Stat) -> c_int {
    _fstat(fd, st)
}

#[no_mangle]
pub extern "C" fn stat(path: *const c_char, st: *mut stat::Stat) -> c_int {
    _stat(path, st)
}

#[no_mangle]
pub extern "C" fn isatty(fd: c_int) -> c_int {
    _isatty(fd)
}

#[no_mangle]
pub extern "C" fn getpid() -> pid_t {
    _getpid()
}

#[no_mangle]
pub extern "C" fn kill(pid: pid_t, sig: c_int) -> c_int {
    _kill(pid, sig)
}

#[no_mangle]
pub extern "C" fn fcntl(fd: c_int, cmd: c_int, arg: c_int) -> c_int {
    fcntl::fcntl_impl(fd, cmd, arg)
}

#[no_mangle]
pub extern "C" fn sbrk(increment: isize) -> *mut c_void {
    _sbrk(increment)
}

// Reentrant forms used by newlib

#[no_mangle]
pub extern "C" fn _open_r(
    _r: *mut c_void,
    path: *const c_char,
    flags: c_int,
    mode: mode_t,
) -> c_int {
    _open(path, flags, mode)
}

#[no_mangle]
pub extern "C" fn _mkdir_r(_r: *mut c_void, path: *const c_char, mode: mode_t) -> c_int {
    _mkdir(path, mode)
}

#[no_mangle]
pub extern "C" fn _rmdir_r(_r: *mut c_void, path: *const c_char) -> c_int {
    _rmdir(path)
}

#[no_mangle]
pub extern "C" fn _rename_r(_r: *mut c_void, old: *const c_char, new: *const c_char) -> c_int {
    _rename(old, new)
}

#[no_mangle]
pub extern "C" fn _unlink_r(_r: *mut c_void, path: *const c_char) -> c_int {
    _unlink(path)
}

#[no_mangle]
pub extern "C" fn _close_r(_r: *mut c_void, fd: c_int) -> c_int {
    _close(fd)
}

#[no_mangle]
pub extern "C" fn _dup_r(_r: *mut c_void, fd: c_int) -> c_int {
    _dup(fd)
}

#[no_mangle]
pub extern "C" fn _dup2_r(_r: *mut c_void, oldfd: c_int, newfd: c_int) -> c_int {
    _dup2(oldfd, newfd)
}

#[no_mangle]
pub extern "C" fn _read_r(_r: *mut c_void, fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
    _read(fd, buf, count)
}

#[no_mangle]
pub extern "C" fn _write_r(
    _r: *mut c_void,
    fd: c_int,
    buf: *const c_void,
    count: size_t,
) -> ssize_t {
    _write(fd, buf, count)
}

#[no_mangle]
pub extern "C" fn _lseek_r(_r: *mut c_void, fd: c_int, offset: off_t, whence: c_int) -> off_t {
    _lseek(fd, offset, whence)
}

#[no_mangle]
pub extern "C" fn _fstat_r(_r: *mut c_void, fd: c_int, st: *mut stat::Stat) -> c_int {
    _fstat(fd, st)
}

#[no_mangle]
pub extern "C" fn _stat_r(_r: *mut c_void, path: *const c_char, st: *mut stat::Stat) -> c_int {
    _stat(path, st)
}

#[no_mangle]
pub extern "C" fn _isatty_r(_r: *mut c_void, fd: c_int) -> c_int {
    _isatty(fd)
}

#[no_mangle]
pub extern "C" fn _getpid_r(_r: *mut c_void) -> pid_t {
    _getpid()
}

#[no_mangle]
pub extern "C" fn _kill_r(_r: *mut c_void, pid: pid_t, sig: c_int) -> c_int {
    _kill(pid, sig)
}

#[no_mangle]
pub extern "C" fn _fcntl_r(_r: *mut c_void, fd: c_int, cmd: c_int, arg: c_int) -> c_int {
    fcntl::fcntl_impl(fd, cmd, arg)
}

#[no_mangle]
pub extern "C" fn _ioctl_r(
    _r: *mut c_void,
    fd: c_int,
    request: c_ulong,
    argp: *mut c_void,
) -> c_int {
    unsafe { termios::_ioctl(fd, request, argp) }
}

#[no_mangle]
pub extern "C" fn _sbrk_r(_r: *mut c_void, increment: isize) -> *mut c_void {
    _sbrk(increment)
}

#[no_mangle]
pub extern "C" fn _mmap_r(
    _r: *mut c_void,
    addr: *mut c_void,
    length: size_t,
    prot: c_int,
    flags: c_int,
    fd: c_int,
    offset: off_t,
) -> *mut c_void {
    _mmap(addr, length, prot, flags, fd, offset)
}

#[no_mangle]
pub extern "C" fn _munmap_r(_r: *mut c_void, addr: *mut c_void, length: size_t) -> c_int {
    _munmap(addr, length)
}
