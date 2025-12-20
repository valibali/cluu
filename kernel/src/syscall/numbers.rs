/*
 * System Call Numbers
 *
 * This module defines the syscall numbers following the System V AMD64 ABI
 * convention used by Linux and most Unix-like systems.
 *
 * Syscall Mechanism:
 * - RAX register contains syscall number
 * - Arguments in RDI, RSI, RDX, R10, R8, R9 (6 args max)
 * - Return value in RAX (signed: negative = error code)
 *
 * Why these numbers:
 * - Compatibility with newlib C library expectations
 * - Standard Linux syscall numbers where applicable
 * - Custom numbers (>= 1000) for CLUU-specific syscalls
 */

/// Syscall error codes (negative values returned in RAX)
pub const ENOSYS: isize = 38;   // Function not implemented
pub const EBADF: isize = 9;     // Bad file descriptor
pub const EFAULT: isize = 14;   // Bad address (invalid pointer from userspace)
pub const EINVAL: isize = 22;   // Invalid argument
pub const ENOMEM: isize = 12;   // Out of memory
pub const ESPIPE: isize = 29;   // Illegal seek (e.g., seek on TTY)

/// Group A: Console I/O syscalls (required for basic userspace)
///
/// These syscalls provide the minimum I/O functionality needed for
/// newlib's stdio to work (printf, scanf, etc.)

/// Read from file descriptor
///
/// Arguments: (fd: i32, buf: *mut u8, count: usize) -> isize
/// Returns: number of bytes read, or negative error code
pub const SYS_READ: usize = 0;

/// Write to file descriptor
///
/// Arguments: (fd: i32, buf: *const u8, count: usize) -> isize
/// Returns: number of bytes written, or negative error code
pub const SYS_WRITE: usize = 1;

/// Close file descriptor
///
/// Arguments: (fd: i32) -> isize
/// Returns: 0 on success, or negative error code
pub const SYS_CLOSE: usize = 3;

/// Get file status
///
/// Arguments: (fd: i32, statbuf: *mut Stat) -> isize
/// Returns: 0 on success, or negative error code
pub const SYS_FSTAT: usize = 5;

/// Seek to position in file
///
/// Arguments: (fd: i32, offset: i64, whence: i32) -> isize
/// Returns: new file position, or negative error code
pub const SYS_LSEEK: usize = 8;

/// Check if file descriptor is a TTY
///
/// Arguments: (fd: i32) -> isize
/// Returns: 1 if TTY, 0 if not, or negative error code
pub const SYS_ISATTY: usize = 16;

/// Group B: Heap management syscall (required for malloc/new)

/// Set program break (heap boundary)
///
/// Arguments: (addr: *mut u8) -> isize
/// Returns: new break on success, or negative error code
///
/// Note: Physical pages are allocated lazily on first access (page fault)
pub const SYS_BRK: usize = 12;

/// Process control syscalls

/// Exit current process
///
/// Arguments: (status: i32) -> !
/// Does not return
pub const SYS_EXIT: usize = 60;

/// Yield CPU to scheduler
///
/// Arguments: () -> isize
/// Returns: 0 on success
pub const SYS_YIELD: usize = 158;  // sched_yield in Linux
