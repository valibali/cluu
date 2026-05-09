//! POSIX errno constants and global errno variable.
//!
//! This module provides standard POSIX error codes for newlib compatibility.
//! The global `ERRNO` variable is used by syscall stubs to report errors.

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use lazy_static::lazy_static;
use spin::Mutex;

// ═══════════════════════════════════════════════════════════════════════════
// POSIX errno constants
// ═══════════════════════════════════════════════════════════════════════════

pub const EPERM: i32 = 1; // Operation not permitted
pub const ENOENT: i32 = 2; // No such file or directory
pub const ESRCH: i32 = 3; // No such process
pub const EINTR: i32 = 4; // Interrupted system call
pub const EIO: i32 = 5; // I/O error
pub const ENXIO: i32 = 6; // No such device or address
pub const E2BIG: i32 = 7; // Argument list too long
pub const ENOEXEC: i32 = 8; // Exec format error
pub const EBADF: i32 = 9; // Bad file number
pub const ECHILD: i32 = 10; // No child processes
pub const EAGAIN: i32 = 11; // Try again
pub const ENOMEM: i32 = 12; // Out of memory
pub const EACCES: i32 = 13; // Permission denied
pub const EFAULT: i32 = 14; // Bad address
pub const ENOTBLK: i32 = 15; // Block device required
pub const EBUSY: i32 = 16; // Device or resource busy
pub const EEXIST: i32 = 17; // File exists
pub const EXDEV: i32 = 18; // Cross-device link
pub const ENODEV: i32 = 19; // No such device
pub const ENOTDIR: i32 = 20; // Not a directory
pub const EISDIR: i32 = 21; // Is a directory
pub const EINVAL: i32 = 22; // Invalid argument
pub const ENFILE: i32 = 23; // File table overflow
pub const EMFILE: i32 = 24; // Too many open files
pub const ENOTTY: i32 = 25; // Not a typewriter
pub const ETXTBSY: i32 = 26; // Text file busy
pub const EFBIG: i32 = 27; // File too large
pub const ENOSPC: i32 = 28; // No space left on device
pub const ESPIPE: i32 = 29; // Illegal seek
pub const EROFS: i32 = 30; // Read-only file system
pub const EMLINK: i32 = 31; // Too many links
pub const EPIPE: i32 = 32; // Broken pipe
pub const EDOM: i32 = 33; // Math argument out of domain
pub const ERANGE: i32 = 34; // Math result not representable
pub const EDEADLK: i32 = 35; // Resource deadlock would occur
pub const ENAMETOOLONG: i32 = 36; // File name too long
pub const ENOLCK: i32 = 37; // No record locks available
pub const ENOSYS: i32 = 38; // Function not implemented
pub const ENOTEMPTY: i32 = 39; // Directory not empty
pub const ELOOP: i32 = 40; // Too many symbolic links
pub const EWOULDBLOCK: i32 = EAGAIN; // Operation would block
pub const ENOMSG: i32 = 42; // No message of desired type
pub const EIDRM: i32 = 43; // Identifier removed
pub const ENOSTR: i32 = 60; // Device not a stream
pub const ENODATA: i32 = 61; // No data available
pub const ETIME: i32 = 62; // Timer expired
pub const ENOSR: i32 = 63; // Out of streams resources
pub const ENOLINK: i32 = 67; // Link has been severed
pub const EPROTO: i32 = 71; // Protocol error
pub const EBADMSG: i32 = 74; // Not a data message
pub const EOVERFLOW: i32 = 75; // Value too large for data type
pub const EILSEQ: i32 = 84; // Illegal byte sequence
pub const ENOTSOCK: i32 = 88; // Socket operation on non-socket
pub const EDESTADDRREQ: i32 = 89; // Destination address required
pub const EMSGSIZE: i32 = 90; // Message too long
pub const EPROTOTYPE: i32 = 91; // Protocol wrong type for socket
pub const ENOPROTOOPT: i32 = 92; // Protocol not available
pub const EPROTONOSUPPORT: i32 = 93; // Protocol not supported
pub const EOPNOTSUPP: i32 = 95; // Operation not supported
pub const EAFNOSUPPORT: i32 = 97; // Address family not supported
pub const EADDRINUSE: i32 = 98; // Address already in use
pub const EADDRNOTAVAIL: i32 = 99; // Cannot assign requested address
pub const ENETDOWN: i32 = 100; // Network is down
pub const ENETUNREACH: i32 = 101; // Network is unreachable
pub const ENETRESET: i32 = 102; // Network dropped connection
pub const ECONNABORTED: i32 = 103; // Software caused connection abort
pub const ECONNRESET: i32 = 104; // Connection reset by peer
pub const ENOBUFS: i32 = 105; // No buffer space available
pub const EISCONN: i32 = 106; // Transport endpoint is connected
pub const ENOTCONN: i32 = 107; // Transport endpoint not connected
pub const ETIMEDOUT: i32 = 110; // Connection timed out
pub const ECONNREFUSED: i32 = 111; // Connection refused
pub const EHOSTUNREACH: i32 = 113; // No route to host
pub const EALREADY: i32 = 114; // Operation already in progress
pub const EINPROGRESS: i32 = 115; // Operation now in progress

// ═══════════════════════════════════════════════════════════════════════════
// Per-thread errno storage
// ═══════════════════════════════════════════════════════════════════════════

lazy_static! {
    /// Per-thread errno cells keyed by current thread token.
    ///
    /// In CLUU userspace, `token_self()` uniquely identifies the current thread.
    pub(crate) static ref ERRNO_BY_THREAD: Mutex<BTreeMap<usize, Box<i32>>> = Mutex::new(BTreeMap::new());
}

#[inline]
fn errno_key() -> usize {
    crate::boot::token_self()
}

/// Set the global errno value.
#[inline]
pub fn set_errno(e: i32) {
    let key = errno_key();
    let mut table = ERRNO_BY_THREAD.lock();
    let cell = table.entry(key).or_insert_with(|| Box::new(0));
    **cell = e;
}

/// Get the current errno value.
#[inline]
pub fn errno() -> i32 {
    let key = errno_key();
    let table = ERRNO_BY_THREAD.lock();
    table.get(&key).map(|v| **v).unwrap_or(0)
}

/// Clear errno (set to 0).
#[inline]
pub fn clear_errno() {
    let key = errno_key();
    let mut table = ERRNO_BY_THREAD.lock();
    let cell = table.entry(key).or_insert_with(|| Box::new(0));
    **cell = 0;
}

/// C-compatible errno access for newlib.
///
/// Newlib calls `__errno()` to get a pointer to errno.
#[no_mangle]
pub extern "C" fn __errno() -> *mut i32 {
    let key = errno_key();
    let mut table = ERRNO_BY_THREAD.lock();
    let cell = table.entry(key).or_insert_with(|| Box::new(0));
    &mut **cell as *mut i32
}

// ═══════════════════════════════════════════════════════════════════════════
// Error conversion from CLUU errors
// ═══════════════════════════════════════════════════════════════════════════

/// Convert a CLUU Error to a POSIX errno value.
pub fn from_cluu_error(err: crate::Error) -> i32 {
    match err {
        crate::Error::InvalidArgument => EINVAL,
        crate::Error::InvalidAddress => EFAULT,
        crate::Error::OutOfMemory => ENOMEM,
        crate::Error::NotFound => ENOENT,
        crate::Error::PermissionDenied => EPERM,
        crate::Error::AlreadyExists => EEXIST,
        crate::Error::Timeout => ETIMEDOUT,
        crate::Error::InvalidOperation => EINVAL,
        crate::Error::InvalidState => EINVAL,
        crate::Error::BufferTooSmall => ERANGE,
        crate::Error::Overflow => EOVERFLOW,
        crate::Error::WouldBlock => EAGAIN,
        crate::Error::NotImplemented => ENOSYS,
        crate::Error::Busy => EBUSY,
        crate::Error::InvalidParameter => EINVAL,
        crate::Error::TooManyLinks => ELOOP,
        crate::Error::Unknown => EIO,
    }
}

/// Set errno from a CLUU Error and return -1.
///
/// Common pattern for syscall stubs:
/// ```rust,ignore
/// match some_operation() {
///     Ok(result) => result,
///     Err(e) => return_error(e),
/// }
/// ```
#[inline]
pub fn return_error(err: crate::Error) -> isize {
    set_errno(from_cluu_error(err));
    -1
}

/// Set errno from a CLUU Error and return -1 as i32.
#[inline]
pub fn return_error_i32(err: crate::Error) -> i32 {
    set_errno(from_cluu_error(err));
    -1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_errno_set_get() {
        set_errno(0);
        assert_eq!(errno(), 0);

        set_errno(EINVAL);
        assert_eq!(errno(), EINVAL);

        set_errno(ENOENT);
        assert_eq!(errno(), ENOENT);

        clear_errno();
        assert_eq!(errno(), 0);
    }

    #[test]
    fn test_errno_constants() {
        assert_eq!(EPERM, 1);
        assert_eq!(ENOENT, 2);
        assert_eq!(EINVAL, 22);
        assert_eq!(ENOSYS, 38);
        assert_eq!(EWOULDBLOCK, EAGAIN);
    }
}
