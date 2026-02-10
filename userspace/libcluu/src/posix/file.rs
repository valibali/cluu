//! File I/O syscall stubs.

use super::{c_char, c_int, c_void, mode_t, off_t, size_t, ssize_t};
use crate::errno::{
    from_cluu_error, return_error, set_errno, EBADF, EINVAL, ENOENT, ENOSYS, ESPIPE,
};
use crate::fd_table::{FdCaps, FdEntry, FD_TABLE};
use crate::ipc::{TTY_READ_REQUEST_LABEL, TTY_WRITE_LABEL};
use crate::types::Message;
use core::slice;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

const GRANT_BUF_BASE: usize = 0x5000_0000;
const GRANT_BUF_SIZE: usize = 64 * 1024;
static GRANT_BUF_READY: AtomicBool = AtomicBool::new(false);

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

            let mut entry = FdEntry::file(vfs_endpoint, vfs_file.fd, client_id, readable, writable);
            entry.file_size = Some(vfs_file.size);
            if (flags & O_APPEND) != 0 {
                entry.position = vfs_file.size as u64;
            }
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

/// Duplicate a file descriptor.
#[no_mangle]
pub extern "C" fn _dup(oldfd: c_int) -> c_int {
    let mut table = FD_TABLE.lock();
    match table.dup(oldfd) {
        Some(fd) => fd,
        None => {
            set_errno(EBADF);
            -1
        }
    }
}

/// Duplicate a file descriptor to a specific number.
#[no_mangle]
pub extern "C" fn _dup2(oldfd: c_int, newfd: c_int) -> c_int {
    if newfd < 0 {
        set_errno(EINVAL);
        return -1;
    }
    let mut table = FD_TABLE.lock();
    match table.dup2(oldfd, newfd) {
        Some(fd) => fd,
        None => {
            set_errno(EBADF);
            -1
        }
    }
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
        read_vfs(fd, &entry, buffer)
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
        write_vfs(fd, &entry, buffer)
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
            let size = match entry.file_size {
                Some(size) => size as u64,
                None => match vfs_fstat(entry) {
                    Some(size) => size as u64,
                    None => {
                        set_errno(ENOSYS);
                        return -1;
                    }
                },
            };
            if offset < 0 {
                size.saturating_sub((-offset) as u64)
            } else {
                size.saturating_add(offset as u64)
            }
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

/// Cached TTY endpoint (from fd 1 = stdout = tty_main).
///
/// fd 0's endpoint is the process's OWN receive endpoint (created by procmgr),
/// not the TTY's. We use fd 1's endpoint which points to tty_main.
static TTY_ENDPOINT: AtomicUsize = AtomicUsize::new(0);

/// Get the TTY's actual endpoint by looking up fd 1 (stdout).
pub fn get_tty_endpoint() -> Option<usize> {
    let cached = TTY_ENDPOINT.load(Ordering::Relaxed);
    if cached != 0 {
        return Some(cached);
    }
    let table = FD_TABLE.lock();
    let ep = table.get(1)?.endpoint; // fd 1 = stdout = tty_main
    TTY_ENDPOINT.store(ep, Ordering::Relaxed);
    Some(ep)
}

fn read_tty(_stdin_endpoint: usize, buffer: &mut [u8]) -> ssize_t {
    // Use the TTY's actual endpoint (fd 1), not fd 0's endpoint which is
    // the process's own receive endpoint and would deadlock on ipc_call.
    let tty_ep = match get_tty_endpoint() {
        Some(ep) => ep,
        None => {
            set_errno(EBADF);
            return -1;
        }
    };

    let mut msg = Message::new(TTY_READ_REQUEST_LABEL, [0; 6], 1);
    msg.words[0] = buffer.len();

    let mut reply_buf = [0u8; 512];
    match crate::syscall::ipc_call(tty_ep, msg.as_bytes(), &mut reply_buf) {
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

fn ensure_grant_buffer() -> Option<usize> {
    if GRANT_BUF_READY.load(Ordering::SeqCst) {
        return Some(GRANT_BUF_BASE);
    }

    let space_token = crate::boot::space_token();
    if space_token == 0 {
        set_errno(ENOSYS);
        return None;
    }

    let pages = (GRANT_BUF_SIZE + crate::mem::PAGE_SIZE - 1) / crate::mem::PAGE_SIZE;
    match crate::syscall::space_map_range(space_token, GRANT_BUF_BASE, 0, 0x03, pages, 0) {
        Ok(_) | Err(crate::Error::AlreadyExists) => {
            GRANT_BUF_READY.store(true, Ordering::SeqCst);
            Some(GRANT_BUF_BASE)
        }
        Err(e) => {
            set_errno(from_cluu_error(e));
            None
        }
    }
}

fn read_vfs(fd: c_int, entry: &FdEntry, buffer: &mut [u8]) -> ssize_t {
    let base = match ensure_grant_buffer() {
        Some(base) => base,
        None => return -1,
    };

    let remote_fd = match entry.remote_fd {
        Some(fd) => fd,
        None => {
            set_errno(EBADF);
            return -1;
        }
    };

    let vfs_client = crate::fs::client::VfsClient::new(entry.endpoint, entry.client_id);
    let mut total = 0usize;
    let mut offset = entry.position as usize;

    while total < buffer.len() {
        let chunk = (buffer.len() - total).min(GRANT_BUF_SIZE);
        let vfs_file = crate::fs::client::VfsFile {
            fd: remote_fd,
            size: 0,
        };
        let grant = match vfs_client.read_grant(
            vfs_file,
            offset,
            chunk,
            crate::boot::space_token(),
            base,
        ) {
            Ok(grant) => grant,
            Err(e) => return return_error(e) as ssize_t,
        };

        if grant.len == 0 {
            break;
        }

        let src =
            unsafe { slice::from_raw_parts((grant.base + grant.offset) as *const u8, grant.len) };
        let to_copy = src.len().min(buffer.len() - total);
        buffer[total..total + to_copy].copy_from_slice(&src[..to_copy]);
        total += to_copy;
        offset += to_copy;

        if to_copy < chunk {
            break;
        }
    }

    if total > 0 {
        let mut table = FD_TABLE.lock();
        if let Some(current) = table.get_mut(fd) {
            current.position = current.position.saturating_add(total as u64);
        }
    }

    total as ssize_t
}

fn write_vfs(fd: c_int, entry: &FdEntry, buffer: &[u8]) -> ssize_t {
    let remote_fd = match entry.remote_fd {
        Some(fd) => fd,
        None => {
            set_errno(EBADF);
            return -1;
        }
    };

    let vfs_client = crate::fs::client::VfsClient::new(entry.endpoint, entry.client_id);
    let vfs_file = crate::fs::client::VfsFile {
        fd: remote_fd,
        size: 0,
    };

    let offset = entry.position as usize;
    let written = match vfs_client.write(vfs_file, offset, buffer) {
        Ok(bytes) => bytes,
        Err(e) => return return_error(e) as ssize_t,
    };

    if written > 0 {
        let mut table = FD_TABLE.lock();
        if let Some(current) = table.get_mut(fd) {
            current.position = current.position.saturating_add(written as u64);
            current.file_size = Some(
                current
                    .file_size
                    .unwrap_or(0)
                    .max(current.position as usize),
            );
        }
    }

    written as ssize_t
}

fn vfs_fstat(entry: &mut FdEntry) -> Option<usize> {
    let remote_fd = entry.remote_fd?;
    let vfs_client = crate::fs::client::VfsClient::new(entry.endpoint, entry.client_id);
    let vfs_file = crate::fs::client::VfsFile {
        fd: remote_fd,
        size: 0,
    };
    match vfs_client.fstat(vfs_file) {
        Ok(stat) => {
            entry.file_size = Some(stat.size);
            entry.file_mode = Some(stat.mode);
            Some(stat.size)
        }
        Err(_) => None,
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

/// Make directory - not supported yet.
#[no_mangle]
pub extern "C" fn _mkdir(_path: *const c_char, _mode: mode_t) -> c_int {
    set_errno(ENOSYS);
    -1
}

/// Remove directory - not supported yet.
#[no_mangle]
pub extern "C" fn _rmdir(_path: *const c_char) -> c_int {
    set_errno(ENOSYS);
    -1
}

/// Rename path - not supported yet.
#[no_mangle]
pub extern "C" fn _rename(_old: *const c_char, _new: *const c_char) -> c_int {
    set_errno(ENOSYS);
    -1
}
