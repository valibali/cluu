//! Directory operations and current working directory for newlib compatibility.
//!
//! - `opendir` / `readdir` / `closedir`: wraps the VFS client readdir API
//! - `getcwd` / `chdir`: tracks a per-process CWD string

use super::{c_char, c_int, off_t, size_t};
use crate::errno::{set_errno, EBADF, EINVAL, ENOENT, ENOTDIR, ERANGE};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr;
use spin::Mutex;

// ═══════════════════════════════════════════════════════════════════════════
// dirent struct (Linux-compatible layout expected by newlib)
// ═══════════════════════════════════════════════════════════════════════════

const NAME_MAX: usize = 255;

/// Directory entry type constants.
pub const DT_UNKNOWN: u8 = 0;
pub const DT_REG: u8 = 8;
pub const DT_DIR: u8 = 4;

/// C-compatible directory entry.
#[repr(C)]
pub struct Dirent {
    pub d_ino: u64,
    pub d_off: off_t,
    pub d_reclen: u16,
    pub d_type: u8,
    pub d_name: [c_char; NAME_MAX + 1],
}

/// Opaque DIR handle wrapping a cached readdir result.
pub struct DIR {
    entries: Vec<crate::fs::client::VfsDirEntry>,
    index: usize,
    current: Dirent,
}

// ═══════════════════════════════════════════════════════════════════════════
// opendir / readdir / closedir
// ═══════════════════════════════════════════════════════════════════════════

/// Open a directory stream.
///
/// Calls the VFS readdir IPC to fetch all entries, then caches them.
/// Returns NULL on error (errno set).
#[no_mangle]
pub extern "C" fn opendir(path: *const c_char) -> *mut DIR {
    if path.is_null() {
        set_errno(EINVAL);
        return ptr::null_mut();
    }

    let path_str = match cstr_to_str(path) {
        Some(s) => s,
        None => {
            set_errno(EINVAL);
            return ptr::null_mut();
        }
    };

    // Resolve relative paths against CWD
    let resolved = resolve_path(path_str);

    let vfs_endpoint = match crate::registry::lookup_service("vfs:main") {
        Some(ep) => ep,
        None => {
            set_errno(ENOENT);
            return ptr::null_mut();
        }
    };

    let client_id = crate::registry::control_endpoint();
    if client_id == 0 {
        set_errno(EINVAL);
        return ptr::null_mut();
    }

    let client = crate::fs::client::VfsClient::new(vfs_endpoint, client_id);
    let entries = match client.readdir(&resolved) {
        Ok(entries) => entries,
        Err(e) => {
            set_errno(crate::errno::from_cluu_error(e));
            return ptr::null_mut();
        }
    };

    let dir = Box::new(DIR {
        entries,
        index: 0,
        current: zeroed_dirent(),
    });

    Box::into_raw(dir)
}

/// Read the next directory entry.
///
/// Returns a pointer to an internal dirent (valid until next readdir/closedir call),
/// or NULL when the end of the directory is reached.
#[no_mangle]
pub extern "C" fn readdir(dirp: *mut DIR) -> *mut Dirent {
    if dirp.is_null() {
        set_errno(EBADF);
        return ptr::null_mut();
    }

    let dir = unsafe { &mut *dirp };

    if dir.index >= dir.entries.len() {
        return ptr::null_mut(); // end of directory (no errno change)
    }

    let entry = &dir.entries[dir.index];
    dir.index += 1;

    // Fill the current dirent
    dir.current = zeroed_dirent();
    dir.current.d_ino = dir.index as u64; // synthetic inode
    dir.current.d_off = dir.index as off_t;
    dir.current.d_type = if entry.is_dir { DT_DIR } else { DT_REG };

    // Copy name (truncate if longer than NAME_MAX)
    let name_bytes = entry.name.as_bytes();
    let copy_len = name_bytes.len().min(NAME_MAX);
    for (i, &byte) in name_bytes.iter().enumerate().take(copy_len) {
        dir.current.d_name[i] = byte as c_char;
    }
    dir.current.d_name[copy_len] = 0;
    dir.current.d_reclen = (core::mem::size_of::<Dirent>()) as u16;

    &mut dir.current as *mut Dirent
}

/// Close a directory stream and free resources.
#[no_mangle]
pub extern "C" fn closedir(dirp: *mut DIR) -> c_int {
    if dirp.is_null() {
        set_errno(EBADF);
        return -1;
    }

    unsafe {
        drop(Box::from_raw(dirp));
    }
    0
}

// ═══════════════════════════════════════════════════════════════════════════
// getcwd / chdir
// ═══════════════════════════════════════════════════════════════════════════

/// Global current working directory, initialized to "/".
static CWD: Mutex<Option<String>> = Mutex::new(None);

/// Initialize the CWD to "/". Called from `__cluu_init`.
pub fn init_cwd() {
    let mut cwd = CWD.lock();
    if cwd.is_none() {
        *cwd = Some(String::from("/"));
    }
}

/// Get current working directory.
///
/// Copies the CWD into `buf` (up to `size` bytes including NUL).
/// Returns `buf` on success, NULL on error (errno set).
#[no_mangle]
pub extern "C" fn getcwd(buf: *mut c_char, size: size_t) -> *mut c_char {
    if buf.is_null() || size == 0 {
        set_errno(EINVAL);
        return ptr::null_mut();
    }

    let cwd = CWD.lock();
    let cwd_str = cwd.as_deref().unwrap_or("/");

    let needed = cwd_str.len() + 1; // +1 for NUL
    if needed > size {
        set_errno(ERANGE);
        return ptr::null_mut();
    }

    unsafe {
        ptr::copy_nonoverlapping(cwd_str.as_ptr() as *const c_char, buf, cwd_str.len());
        *buf.add(cwd_str.len()) = 0;
    }

    buf
}

/// Change current working directory.
///
/// Validates the path exists (via VFS stat) and updates the global CWD.
#[no_mangle]
pub extern "C" fn chdir(path: *const c_char) -> c_int {
    if path.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let path_str = match cstr_to_str(path) {
        Some(s) => s,
        None => {
            set_errno(EINVAL);
            return -1;
        }
    };

    let resolved = resolve_path(path_str);

    // Validate path exists and is a directory via VFS stat
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
    let client = crate::fs::client::VfsClient::new(vfs_endpoint, client_id);
    match client.stat(&resolved) {
        Ok(info) => {
            // Check if it's a directory (S_IFDIR = 0o040000)
            if info.mode & 0o170000 != 0o040000 {
                set_errno(ENOTDIR);
                return -1;
            }
        }
        Err(e) => {
            set_errno(crate::errno::from_cluu_error(e));
            return -1;
        }
    }

    let mut cwd = CWD.lock();
    *cwd = Some(String::from(resolved.as_str()));
    0
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

fn zeroed_dirent() -> Dirent {
    Dirent {
        d_ino: 0,
        d_off: 0,
        d_reclen: 0,
        d_type: DT_UNKNOWN,
        d_name: [0; NAME_MAX + 1],
    }
}

/// Convert a C string pointer to a Rust &str.
fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe {
        let mut len = 0;
        let mut p = ptr;
        while *p != 0 {
            len += 1;
            p = p.add(1);
        }
        core::str::from_utf8(core::slice::from_raw_parts(ptr as *const u8, len)).ok()
    }
}

/// Resolve a path: if relative, prepend CWD. Normalize trailing slashes.
fn resolve_path(path: &str) -> String {
    if path.starts_with('/') {
        // Absolute path — use as-is, but normalize trailing slash
        let mut s = String::from(path);
        while s.len() > 1 && s.ends_with('/') {
            s.pop();
        }
        return s;
    }

    // Relative path — prepend CWD
    let cwd = CWD.lock();
    let base = cwd.as_deref().unwrap_or("/");

    let mut result = String::from(base);
    if !result.ends_with('/') {
        result.push('/');
    }
    result.push_str(path);

    // Normalize trailing slash
    while result.len() > 1 && result.ends_with('/') {
        result.pop();
    }
    result
}
