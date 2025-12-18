/*
 * Device Abstraction Layer
 *
 * TTY-specific device trait that will be generalized to a File trait
 * when adding regular filesystem support (ext2).
 *
 * This provides a simple abstraction for character devices (TTYs)
 * with minimal POSIX compatibility.
 */

/// Device trait for TTY and future file abstraction
///
/// This trait will be renamed to `File` when adding regular filesystem support.
/// For now, it's focused on TTY devices with minimal complexity.
pub trait Device: Send + Sync {
    /// Read up to buf.len() bytes into buf
    ///
    /// Returns the number of bytes read, or an error.
    /// May block until data is available (for TTYs).
    fn read(&self, buf: &mut [u8]) -> Result<usize, Errno>;

    /// Write buf.len() bytes from buf
    ///
    /// Returns the number of bytes written, or an error.
    fn write(&self, buf: &[u8]) -> Result<usize, Errno>;

    /// Device control operation (ioctl)
    ///
    /// Used for TTY control (termios), will be extended for other devices.
    fn ioctl(&self, request: u32, arg: usize) -> Result<i32, Errno>;

    /// Check if device is a TTY
    ///
    /// Returns true for TTY devices, false for regular files.
    fn is_tty(&self) -> bool {
        false
    }

    /// Get device metadata
    ///
    /// Returns file type (S_IFCHR for TTY, S_IFREG for file) and size.
    fn stat(&self) -> Stat;

    /// Seek to position (default: not seekable)
    ///
    /// TTYs return ESPIPE. Regular files will override this.
    fn seek(&self, _offset: i64, _whence: i32) -> Result<i64, Errno> {
        Err(Errno::ESPIPE)
    }
}

/// POSIX errno values
///
/// Subset of standard POSIX error codes for syscall compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Errno {
    EPERM = 1,   // Operation not permitted
    ENOENT = 2,  // No such file or directory
    EINTR = 4,   // Interrupted system call
    EIO = 5,     // I/O error
    EBADF = 9,   // Bad file descriptor
    EAGAIN = 11, // Try again
    ENOMEM = 12, // Out of memory
    EACCES = 13, // Permission denied
    EFAULT = 14, // Bad address
    EINVAL = 22, // Invalid argument
    ENOTTY = 25, // Not a typewriter
    ESPIPE = 29, // Illegal seek
}

/// Minimal stat structure
///
/// Subset of POSIX struct stat, focusing on st_mode for type checking.
/// This allows _isatty() to be implemented via _fstat() + S_ISCHR().
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Stat {
    pub st_mode: u32,    // File type and mode
    pub st_size: u64,    // File size in bytes
    pub st_blksize: u64, // Block size for I/O
    pub st_blocks: u64,  // Number of 512B blocks allocated
}

impl Default for Stat {
    fn default() -> Self {
        Self {
            st_mode: 0,
            st_size: 0,
            st_blksize: 0,
            st_blocks: 0,
        }
    }
}

// File type constants (POSIX)
pub const S_IFMT: u32 = 0o170000; // File type mask
pub const S_IFCHR: u32 = 0o020000; // Character device
pub const S_IFREG: u32 = 0o100000; // Regular file
pub const S_IFDIR: u32 = 0o040000; // Directory
pub const S_IFIFO: u32 = 0o010000; // FIFO/pipe

// Seek whence constants (POSIX)
pub const SEEK_SET: i32 = 0; // Seek from beginning
pub const SEEK_CUR: i32 = 1; // Seek from current position
pub const SEEK_END: i32 = 2; // Seek from end

/// Check if mode indicates a character device
#[inline]
pub fn S_ISCHR(mode: u32) -> bool {
    (mode & S_IFMT) == S_IFCHR
}

/// Check if mode indicates a regular file
#[inline]
pub fn S_ISREG(mode: u32) -> bool {
    (mode & S_IFMT) == S_IFREG
}

/// Check if mode indicates a directory
#[inline]
pub fn S_ISDIR(mode: u32) -> bool {
    (mode & S_IFMT) == S_IFDIR
}

/// Check if mode indicates a FIFO
#[inline]
pub fn S_ISFIFO(mode: u32) -> bool {
    (mode & S_IFMT) == S_IFIFO
}