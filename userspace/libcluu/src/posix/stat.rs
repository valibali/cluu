// File status syscall stubs.

use super::{c_char, c_int, mode_t, off_t, time_t};
use crate::errno::{set_errno, EBADF, EINVAL, ENOSYS};
use crate::fd_table::{FdCaps, FD_TABLE};

/// Stat structure (simplified, matches newlib expectations).
#[repr(C)]
pub struct Stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: mode_t,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub st_size: off_t,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: time_t,
    pub st_atime_nsec: i64,
    pub st_mtime: time_t,
    pub st_mtime_nsec: i64,
    pub st_ctime: time_t,
    pub st_ctime_nsec: i64,
}

// File type bits for st_mode
pub const S_IFMT: mode_t = 0o170000; // File type mask
pub const S_IFREG: mode_t = 0o100000; // Regular file
pub const S_IFDIR: mode_t = 0o040000; // Directory
pub const S_IFCHR: mode_t = 0o020000; // Character device
pub const S_IFBLK: mode_t = 0o060000; // Block device
pub const S_IFIFO: mode_t = 0o010000; // FIFO
pub const S_IFLNK: mode_t = 0o120000; // Symbolic link
pub const S_IFSOCK: mode_t = 0o140000; // Socket

// Permission bits
pub const S_IRWXU: mode_t = 0o700; // User rwx
pub const S_IRUSR: mode_t = 0o400; // User read
pub const S_IWUSR: mode_t = 0o200; // User write
pub const S_IXUSR: mode_t = 0o100; // User execute
pub const S_IRWXG: mode_t = 0o070; // Group rwx
pub const S_IRGRP: mode_t = 0o040; // Group read
pub const S_IWGRP: mode_t = 0o020; // Group write
pub const S_IXGRP: mode_t = 0o010; // Group execute
pub const S_IRWXO: mode_t = 0o007; // Other rwx
pub const S_IROTH: mode_t = 0o004; // Other read
pub const S_IWOTH: mode_t = 0o002; // Other write
pub const S_IXOTH: mode_t = 0o001; // Other execute

impl Stat {
    /// Create a zeroed stat structure.
    pub fn zeroed() -> Self {
        Self {
            st_dev: 0,
            st_ino: 0,
            st_mode: 0,
            st_nlink: 1,
            st_uid: 0,
            st_gid: 0,
            st_rdev: 0,
            st_size: 0,
            st_blksize: 512,
            st_blocks: 0,
            st_atime: 0,
            st_atime_nsec: 0,
            st_mtime: 0,
            st_mtime_nsec: 0,
            st_ctime: 0,
            st_ctime_nsec: 0,
        }
    }

    /// Create stat for a character device (TTY).
    pub fn char_device() -> Self {
        let mut s = Self::zeroed();
        s.st_mode = S_IFCHR | S_IRUSR | S_IWUSR;
        s
    }

    /// Create stat for a regular file.
    #[allow(dead_code)]
    pub fn regular_file(size: u64) -> Self {
        let mut s = Self::zeroed();
        s.st_mode = S_IFREG | S_IRUSR | S_IWUSR | S_IRGRP | S_IROTH;
        s.st_size = size as off_t;
        s.st_blocks = ((size + 511) / 512) as i64;
        s
    }

    /// Create stat for a directory.
    #[allow(dead_code)]
    pub fn directory() -> Self {
        let mut s = Self::zeroed();
        s.st_mode = S_IFDIR | S_IRWXU | S_IRGRP | S_IXGRP | S_IROTH | S_IXOTH;
        s.st_nlink = 2;
        s
    }
}

/// Get file status by file descriptor.
///
/// # Arguments
/// - `fd`: File descriptor
/// - `st`: Pointer to stat structure to fill
///
/// # Returns
/// 0 on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn _fstat(fd: c_int, st: *mut Stat) -> c_int {
    if st.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let table = FD_TABLE.lock();
    let entry = match table.get(fd) {
        Some(e) => e,
        None => {
            set_errno(EBADF);
            return -1;
        }
    };

    let stat = if entry.caps.contains(FdCaps::IS_TTY) {
        Stat::char_device()
    } else {
        // For files, we'd need to query VFS for size
        // For now, return a basic regular file stat
        Stat::regular_file(0)
    };

    unsafe {
        *st = stat;
    }
    0
}

/// Get file status by path.
///
/// # Arguments
/// - `path`: Path to file (null-terminated)
/// - `st`: Pointer to stat structure to fill
///
/// # Returns
/// 0 on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn _stat(_path: *const c_char, st: *mut Stat) -> c_int {
    if st.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    // TODO: Query VFS for file info
    // For now, just return ENOSYS
    set_errno(ENOSYS);
    -1
}

/// Check if file descriptor is a terminal.
///
/// # Arguments
/// - `fd`: File descriptor
///
/// # Returns
/// 1 if fd is a TTY, 0 otherwise.
#[no_mangle]
pub extern "C" fn _isatty(fd: c_int) -> c_int {
    let table = FD_TABLE.lock();
    match table.get(fd) {
        Some(entry) if entry.caps.contains(FdCaps::IS_TTY) => 1,
        Some(_) => 0,
        None => {
            set_errno(EBADF);
            0
        }
    }
}
