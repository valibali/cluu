// File status syscall stubs.

use super::{c_char, c_int, mode_t, off_t, time_t};
use crate::errno::{set_errno, EBADF, EINVAL, ENOENT};
use crate::fd_table::{FdCaps, FD_TABLE};

/// Stat structure matching newlib's `struct stat` layout for x86_64-cluu-elf.
///
/// Field types must match newlib's sys/_types.h exactly:
///   dev_t = short (i16), ino_t = unsigned short (u16),
///   nlink_t = unsigned short (u16), uid_t/gid_t = unsigned short (u16),
///   off_t = long (i64), time_t = long (i64),
///   blksize_t = long (i64), blkcnt_t = long (i64).
///
/// Field order: timespec fields come BEFORE blksize/blocks (non-SVR4 path),
/// followed by st_spare4[2].
///
/// Total size: 104 bytes.
#[repr(C)]
pub struct Stat {
    pub st_dev: i16,
    pub st_ino: u16,
    pub st_mode: mode_t,
    pub st_nlink: u16,
    pub st_uid: u16,
    pub st_gid: u16,
    pub st_rdev: i16,
    pub st_size: off_t,
    pub st_atime: time_t,
    pub st_atime_nsec: i64,
    pub st_mtime: time_t,
    pub st_mtime_nsec: i64,
    pub st_ctime: time_t,
    pub st_ctime_nsec: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_spare4: [i64; 2],
}

// Compile-time check: Stat must be exactly 104 bytes to match newlib's struct stat.
const _: () = assert!(core::mem::size_of::<Stat>() == 104);

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
            st_atime: 0,
            st_atime_nsec: 0,
            st_mtime: 0,
            st_mtime_nsec: 0,
            st_ctime: 0,
            st_ctime_nsec: 0,
            st_blksize: 512,
            st_blocks: 0,
            st_spare4: [0; 2],
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
        s.st_blocks = size.div_ceil(512) as i64;
        s.st_blksize = 512;
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

    let mut table = FD_TABLE.lock();
    let entry = match table.get_mut(fd) {
        Some(e) => e,
        None => {
            set_errno(EBADF);
            return -1;
        }
    };

    let stat = if entry.caps.contains(FdCaps::IS_TTY) {
        Stat::char_device()
    } else if let Some(remote_fd) = entry.remote_fd {
        let vfs_client = crate::fs::client::VfsClient::new(entry.endpoint, entry.client_id);
        let vfs_file = crate::fs::client::VfsFile {
            fd: remote_fd,
            size: 0,
        };
        match vfs_client.fstat(vfs_file) {
            Ok(info) => {
                entry.file_size = Some(info.size);
                entry.file_mode = Some(info.mode);
                let mut st = Stat::regular_file(info.size as u64);
                st.st_mode = info.mode as mode_t;
                st
            }
            Err(_) => Stat::regular_file(0),
        }
    } else {
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

    if _path.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let path_str = unsafe {
        let mut len = 0;
        let mut p = _path;
        while *p != 0 {
            len += 1;
            p = p.add(1);
        }
        match core::str::from_utf8(core::slice::from_raw_parts(_path as *const u8, len)) {
            Ok(s) => s,
            Err(_) => {
                set_errno(EINVAL);
                return -1;
            }
        }
    };

    let vfs_endpoint = match crate::registry::lookup_service("vfs:main") {
        Some(ep) => ep,
        None => {
            set_errno(ENOENT);
            return -1;
        }
    };
    let client_id = crate::registry::control_endpoint();
    if client_id == 0 {
        set_errno(EINVAL);
        return -1;
    }

    let vfs_client = crate::fs::client::VfsClient::new(vfs_endpoint, client_id);
    match vfs_client.stat(path_str) {
        Ok(info) => {
            let mut stat = Stat::regular_file(info.size as u64);
            stat.st_mode = info.mode as mode_t;
            unsafe {
                *st = stat;
            }
            0
        }
        Err(e) => {
            set_errno(crate::errno::from_cluu_error(e));
            -1
        }
    }
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
