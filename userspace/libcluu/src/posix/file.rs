//! File I/O syscall stubs.

use super::{c_char, c_int, c_void, mode_t, off_t, size_t, ssize_t};
use crate::errno::{return_error, set_errno, EBADF, EINVAL, ENOENT, ENOSYS, ESPIPE};
use crate::fd_table::{FdCaps, FdEntry, FD_TABLE};
use crate::ipc::{TTY_READ_LABEL, TTY_WRITE_LABEL};
use crate::types::Message;
use core::slice;

// Open flags (matching Linux values)
pub const O_RDONLY: c_int = 0;
pub const O_WRONLY: c_int = 1;
pub const O_RDWR: c_int = 2;
pub const O_CREAT: c_int = 0o100;
pub const O_EXCL: c_int = 0o200;
pub const O_TRUNC: c_int = 0o1000;
pub const O_APPEND: c_int = 0o2000;

// Seek whence values
pub const SEEK_SET: c_int = 0;
pub const SEEK_CUR: c_int = 1;
pub const SEEK_END: c_int = 2;

/// Open a file.
///
/// # Arguments
/// - `path`: Path to file (null-terminated)
/// - `flags`: Open flags (O_RDONLY, O_WRONLY, O_RDWR, etc.)
/// - `mode`: File mode for creation (ignored if O_CREAT not set)
///
/// # Returns
/// File descriptor on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn _open(path: *const c_char, flags: c_int, _mode: mode_t) -> c_int {
    if path.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    // Convert path to Rust str
    let path_str = unsafe {
        let mut len = 0;
        let mut p = path;
        while *p != 0 {
            len += 1;
            p = p.add(1);
        }
        match core::str::from_utf8(slice::from_raw_parts(path as *const u8, len)) {
            Ok(s) => s,
            Err(_) => {
                set_errno(EINVAL);
                return -1;
            }
        }
    };

    // Get VFS endpoint from registry
    let vfs_endpoint = match crate::registry::lookup_service("vfs:main") {
        Some(ep) => ep,
        None => {
            set_errno(ENOENT);
            return -1;
        }
    };

    // Get client ID (use our control endpoint)
    let client_id = crate::registry::control_endpoint();
    if client_id == 0 {
        set_errno(EINVAL);
        return -1;
    }

    // Create VFS client and open file
    let vfs_client = crate::fs::client::VfsClient::new(vfs_endpoint, client_id);
    match vfs_client.open(path_str) {
        Ok(vfs_file) => {
            // Determine capabilities from flags
            let readable = (flags & O_WRONLY) == 0;
            let writable = (flags & (O_WRONLY | O_RDWR)) != 0;

            let entry = FdEntry::file(vfs_endpoint, vfs_file.fd, client_id, readable, writable);
            let fd = FD_TABLE.lock().insert(entry);
            fd
        }
        Err(e) => return_error(e) as c_int,
    }
}

/// Close a file descriptor.
///
/// # Returns
/// 0 on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn _close(fd: c_int) -> c_int {
    let mut table = FD_TABLE.lock();

    let entry = match table.get(fd) {
        Some(e) => e.clone(),
        None => {
            set_errno(EBADF);
            return -1;
        }
    };

    // If it's a VFS file, close it on the server
    if let Some(remote_fd) = entry.remote_fd {
        let vfs_client = crate::fs::client::VfsClient::new(entry.endpoint, entry.client_id);
        let vfs_file = crate::fs::client::VfsFile {
            fd: remote_fd,
            size: 0,
        };
        let _ = vfs_client.close(vfs_file);
    }

    table.remove(fd);
    0
}

/// Read from a file descriptor.
///
/// # Arguments
/// - `fd`: File descriptor
/// - `buf`: Buffer to read into
/// - `count`: Maximum bytes to read
///
/// # Returns
/// Number of bytes read on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn _read(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
    if buf.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let table = FD_TABLE.lock();
    let entry = match table.get(fd) {
        Some(e) => e.clone(),
        None => {
            set_errno(EBADF);
            return -1;
        }
    };
    drop(table);

    if !entry.caps.contains(FdCaps::READ) {
        set_errno(EBADF);
        return -1;
    }

    let buffer = unsafe { slice::from_raw_parts_mut(buf as *mut u8, count) };

    if entry.is_tty() {
        // TTY read via IPC
        read_tty(entry.endpoint, buffer)
    } else if let Some(_remote_fd) = entry.remote_fd {
        // VFS file read - for now use grant-based read
        // TODO: implement proper VFS read with position tracking
        set_errno(ENOSYS);
        -1
    } else {
        set_errno(EBADF);
        -1
    }
}

/// Write to a file descriptor.
///
/// # Arguments
/// - `fd`: File descriptor
/// - `buf`: Buffer to write from
/// - `count`: Number of bytes to write
///
/// # Returns
/// Number of bytes written on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn _write(fd: c_int, buf: *const c_void, count: size_t) -> ssize_t {
    if buf.is_null() && count > 0 {
        set_errno(EINVAL);
        return -1;
    }

    let table = FD_TABLE.lock();
    let entry = match table.get(fd) {
        Some(e) => e.clone(),
        None => {
            set_errno(EBADF);
            return -1;
        }
    };
    drop(table);

    if !entry.caps.contains(FdCaps::WRITE) {
        set_errno(EBADF);
        return -1;
    }

    let buffer = unsafe { slice::from_raw_parts(buf as *const u8, count) };

    if entry.is_tty() {
        // TTY write via IPC
        write_tty(entry.endpoint, buffer)
    } else if let Some(_remote_fd) = entry.remote_fd {
        // VFS file write - not implemented yet
        set_errno(ENOSYS);
        -1
    } else {
        set_errno(EBADF);
        -1
    }
}

/// Seek in a file descriptor.
///
/// # Arguments
/// - `fd`: File descriptor
/// - `offset`: Offset to seek to
/// - `whence`: SEEK_SET, SEEK_CUR, or SEEK_END
///
/// # Returns
/// New file position on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn _lseek(fd: c_int, offset: off_t, whence: c_int) -> off_t {
    let mut table = FD_TABLE.lock();
    let entry = match table.get_mut(fd) {
        Some(e) => e,
        None => {
            set_errno(EBADF);
            return -1;
        }
    };

    if !entry.caps.contains(FdCaps::SEEK) {
        set_errno(ESPIPE); // Illegal seek (e.g., on pipe/tty)
        return -1;
    }

    // For now, just track position locally
    // TODO: Get actual file size for SEEK_END
    let new_pos = match whence {
        SEEK_SET => offset as u64,
        SEEK_CUR => {
            if offset < 0 {
                entry.position.saturating_sub((-offset) as u64)
            } else {
                entry.position.saturating_add(offset as u64)
            }
        }
        SEEK_END => {
            // Would need file size - for now just use current position
            set_errno(ENOSYS);
            return -1;
        }
        _ => {
            set_errno(EINVAL);
            return -1;
        }
    };

    entry.position = new_pos;
    new_pos as off_t
}

// ═══════════════════════════════════════════════════════════════════════════
// Internal helpers
// ═══════════════════════════════════════════════════════════════════════════

fn read_tty(endpoint: usize, buffer: &mut [u8]) -> ssize_t {
    // Use TTY_READ protocol
    let mut msg = Message::new(TTY_READ_LABEL, [0; 6], 1);
    msg.words[0] = buffer.len();

    let mut reply_buf = [0u8; 512];
    match crate::syscall::ipc_call(endpoint, msg.as_bytes(), &mut reply_buf) {
        Ok(bytes) => {
            if bytes <= core::mem::size_of::<Message>() {
                return 0; // No data
            }
            let data_len = bytes - core::mem::size_of::<Message>();
            let to_copy = data_len.min(buffer.len());
            buffer[..to_copy]
                .copy_from_slice(&reply_buf[core::mem::size_of::<Message>()..][..to_copy]);
            to_copy as ssize_t
        }
        Err(e) => crate::errno::return_error(e),
    }
}

fn write_tty(endpoint: usize, buffer: &[u8]) -> ssize_t {
    // Use TTY_WRITE protocol with retry
    match crate::ipc::send_with_retry(endpoint, TTY_WRITE_LABEL, buffer) {
        Ok(()) => buffer.len() as ssize_t,
        Err(e) => crate::errno::return_error(e),
    }
}

/// Link (hard link) - not supported.
#[no_mangle]
pub extern "C" fn _link(_old: *const c_char, _new: *const c_char) -> c_int {
    set_errno(ENOSYS);
    -1
}

/// Unlink (delete file) - not supported yet.
#[no_mangle]
pub extern "C" fn _unlink(_path: *const c_char) -> c_int {
    set_errno(ENOSYS);
    -1
}
