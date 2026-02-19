//! POSIX stub functions for newlib compatibility.
//!
//! These provide minimal implementations of commonly expected POSIX functions
//! that C programs (especially MicroPython) may reference.

use super::{c_char, c_int, c_void, mode_t, size_t};
use crate::errno::{set_errno, EACCES, EINVAL, ENOENT, ENOMEM};
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

// ═══════════════════════════════════════════════════════════════════════════
// File access checks
// ═══════════════════════════════════════════════════════════════════════════

pub const F_OK: c_int = 0;
pub const R_OK: c_int = 4;
pub const W_OK: c_int = 2;
pub const X_OK: c_int = 1;

/// Check file accessibility.
///
/// Uses stat() to determine if the file exists and has the requested permissions.
#[no_mangle]
pub extern "C" fn access(path: *const c_char, mode: c_int) -> c_int {
    if path.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    // Use _stat to check existence
    let mut st = super::stat::Stat::zeroed();
    let rc = super::stat::_stat(path, &mut st);
    if rc != 0 {
        return -1; // errno already set by _stat
    }

    // F_OK just checks existence
    if mode == F_OK {
        return 0;
    }

    // Single-user OS: check permission bits against stat mode
    // We treat the owner bits as authoritative
    if (mode & R_OK) != 0 && (st.st_mode & super::stat::S_IRUSR) == 0 {
        set_errno(EACCES);
        return -1;
    }
    if (mode & W_OK) != 0 && (st.st_mode & super::stat::S_IWUSR) == 0 {
        set_errno(EACCES);
        return -1;
    }
    if (mode & X_OK) != 0 && (st.st_mode & super::stat::S_IXUSR) == 0 {
        set_errno(EACCES);
        return -1;
    }

    0
}

// ═══════════════════════════════════════════════════════════════════════════
// Permission stubs (single-user, no enforcement)
// ═══════════════════════════════════════════════════════════════════════════

#[no_mangle]
pub extern "C" fn chmod(_path: *const c_char, _mode: mode_t) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn fchmod(_fd: c_int, _mode: mode_t) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn chown(_path: *const c_char, _uid: uid_t, _gid: gid_t) -> c_int {
    0
}

// ═══════════════════════════════════════════════════════════════════════════
// User/group identity (always root)
// ═══════════════════════════════════════════════════════════════════════════

/// uid_t in newlib for CLUU is `unsigned short` (u16).
type uid_t = u16;
/// gid_t in newlib for CLUU is `unsigned short` (u16).
type gid_t = u16;

#[no_mangle]
pub extern "C" fn getuid() -> uid_t {
    0
}

#[no_mangle]
pub extern "C" fn geteuid() -> uid_t {
    0
}

#[no_mangle]
pub extern "C" fn getgid() -> gid_t {
    0
}

#[no_mangle]
pub extern "C" fn getegid() -> gid_t {
    0
}

// ═══════════════════════════════════════════════════════════════════════════
// passwd database stubs
// ═══════════════════════════════════════════════════════════════════════════

/// Minimal passwd struct matching POSIX.
#[repr(C)]
pub struct Passwd {
    pub pw_name: *mut c_char,
    pub pw_passwd: *mut c_char,
    pub pw_uid: uid_t,
    pub pw_gid: gid_t,
    pub pw_comment: *mut c_char,
    pub pw_gecos: *mut c_char,
    pub pw_dir: *mut c_char,
    pub pw_shell: *mut c_char,
}

static ROOT_NAME: &[u8] = b"root\0";
static ROOT_PASSWD: &[u8] = b"\0";
static ROOT_COMMENT: &[u8] = b"\0";
static ROOT_GECOS: &[u8] = b"root\0";
static ROOT_DIR: &[u8] = b"/\0";
static ROOT_SHELL: &[u8] = b"/bin/shell\0";

static mut ROOT_PASSWD_STRUCT: Passwd = Passwd {
    pw_name: ptr::null_mut(),
    pw_passwd: ptr::null_mut(),
    pw_uid: 0,
    pw_gid: 0,
    pw_comment: ptr::null_mut(),
    pw_gecos: ptr::null_mut(),
    pw_dir: ptr::null_mut(),
    pw_shell: ptr::null_mut(),
};

fn init_root_passwd() -> *mut Passwd {
    unsafe {
        ROOT_PASSWD_STRUCT.pw_name = ROOT_NAME.as_ptr() as *mut c_char;
        ROOT_PASSWD_STRUCT.pw_passwd = ROOT_PASSWD.as_ptr() as *mut c_char;
        ROOT_PASSWD_STRUCT.pw_uid = 0;
        ROOT_PASSWD_STRUCT.pw_gid = 0;
        ROOT_PASSWD_STRUCT.pw_comment = ROOT_COMMENT.as_ptr() as *mut c_char;
        ROOT_PASSWD_STRUCT.pw_gecos = ROOT_GECOS.as_ptr() as *mut c_char;
        ROOT_PASSWD_STRUCT.pw_dir = ROOT_DIR.as_ptr() as *mut c_char;
        ROOT_PASSWD_STRUCT.pw_shell = ROOT_SHELL.as_ptr() as *mut c_char;
        &raw mut ROOT_PASSWD_STRUCT
    }
}

#[no_mangle]
pub extern "C" fn getpwuid(uid: uid_t) -> *mut Passwd {
    if uid == 0 {
        init_root_passwd()
    } else {
        ptr::null_mut()
    }
}

#[no_mangle]
pub extern "C" fn getpwnam(name: *const c_char) -> *mut Passwd {
    if name.is_null() {
        return ptr::null_mut();
    }
    let name_bytes = unsafe { cstr_bytes(name) };
    if name_bytes == b"root" {
        init_root_passwd()
    } else {
        ptr::null_mut()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// mkstemp
// ═══════════════════════════════════════════════════════════════════════════

static MKSTEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Create a unique temporary file.
///
/// Replaces trailing XXXXXX in template with pid+counter, opens with O_CREAT|O_EXCL|O_RDWR.
#[no_mangle]
pub extern "C" fn mkstemp(template: *mut c_char) -> c_int {
    if template.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    // Find length of template
    let mut len = 0usize;
    unsafe {
        let mut p = template;
        while *p != 0 {
            len += 1;
            p = p.add(1);
        }
    }

    if len < 6 {
        set_errno(EINVAL);
        return -1;
    }

    // Check that last 6 chars are XXXXXX
    let suffix_start = len - 6;
    for i in 0..6 {
        if unsafe { *template.add(suffix_start + i) } != b'X' as c_char {
            set_errno(EINVAL);
            return -1;
        }
    }

    // Generate unique suffix from pid + counter
    let pid = crate::boot::pid() as u32;
    let counter = MKSTEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let unique = pid.wrapping_mul(1000).wrapping_add(counter);

    // Write 6 hex digits
    let hex_chars = b"0123456789abcdef";
    let mut val = unique;
    for i in (0..6).rev() {
        unsafe {
            *template.add(suffix_start + i) = hex_chars[(val & 0xf) as usize] as c_char;
        }
        val >>= 4;
    }

    // Open with O_CREAT | O_EXCL | O_RDWR
    use super::file::{O_CREAT, O_EXCL, O_RDWR};
    super::file::_open(template as *const c_char, O_CREAT | O_EXCL | O_RDWR, 0o600)
}

// ═══════════════════════════════════════════════════════════════════════════
// Path manipulation
// ═══════════════════════════════════════════════════════════════════════════

/// Resolve a pathname to an absolute path with . and .. resolved.
///
/// Pure string normalization (no symlink resolution).
/// If resolved is NULL, allocates with malloc.
#[no_mangle]
pub extern "C" fn realpath(path: *const c_char, resolved: *mut c_char) -> *mut c_char {
    if path.is_null() {
        set_errno(EINVAL);
        return ptr::null_mut();
    }

    let path_bytes = unsafe { cstr_bytes(path) };
    if path_bytes.is_empty() {
        set_errno(ENOENT);
        return ptr::null_mut();
    }

    // Normalize the path
    let normalized = normalize_path(path_bytes);

    let out = if resolved.is_null() {
        // Allocate with malloc
        extern "C" {
            fn malloc(size: size_t) -> *mut c_void;
        }
        let buf = unsafe { malloc(normalized.len() + 1) as *mut c_char };
        if buf.is_null() {
            set_errno(ENOMEM);
            return ptr::null_mut();
        }
        buf
    } else {
        resolved
    };

    unsafe {
        ptr::copy_nonoverlapping(normalized.as_ptr() as *const c_char, out, normalized.len());
        *out.add(normalized.len()) = 0;
    }

    out
}

/// Normalize a path: collapse // and resolve . and .. components.
fn normalize_path(path: &[u8]) -> alloc::vec::Vec<u8> {
    use alloc::vec::Vec;

    let is_absolute = path.first() == Some(&b'/');

    let mut components: Vec<&[u8]> = Vec::new();

    for component in path.split(|&b| b == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }
        if component == b".." {
            if !components.is_empty() {
                components.pop();
            }
            continue;
        }
        components.push(component);
    }

    let mut result = Vec::new();
    if is_absolute {
        result.push(b'/');
    }

    for (i, comp) in components.iter().enumerate() {
        if i > 0 {
            result.push(b'/');
        }
        result.extend_from_slice(comp);
    }

    if result.is_empty() {
        result.push(b'/');
    }

    result
}

/// Return the last component of a path.
///
/// POSIX semantics: may modify the input string.
#[no_mangle]
pub extern "C" fn basename(path: *mut c_char) -> *mut c_char {
    static DOT: &[u8] = b".\0";
    static SLASH: &[u8] = b"/\0";

    if path.is_null() {
        return DOT.as_ptr() as *mut c_char;
    }

    let bytes = unsafe { cstr_bytes(path as *const c_char) };
    if bytes.is_empty() {
        return DOT.as_ptr() as *mut c_char;
    }

    // Strip trailing slashes
    let mut end = bytes.len();
    while end > 1 && bytes[end - 1] == b'/' {
        end -= 1;
    }

    if end == 1 && bytes[0] == b'/' {
        return SLASH.as_ptr() as *mut c_char;
    }

    // Find last slash before end
    let mut start = end;
    while start > 0 && bytes[start - 1] != b'/' {
        start -= 1;
    }

    // Null-terminate at end (modifies input - POSIX semantics)
    unsafe {
        *path.add(end) = 0;
    }

    unsafe { path.add(start) }
}

/// Return the directory portion of a path.
///
/// POSIX semantics: may modify the input string.
#[no_mangle]
pub extern "C" fn dirname(path: *mut c_char) -> *mut c_char {
    static DOT: &[u8] = b".\0";
    static SLASH: &[u8] = b"/\0";

    if path.is_null() {
        return DOT.as_ptr() as *mut c_char;
    }

    let bytes = unsafe { cstr_bytes(path as *const c_char) };
    if bytes.is_empty() {
        return DOT.as_ptr() as *mut c_char;
    }

    // Strip trailing slashes
    let mut end = bytes.len();
    while end > 1 && bytes[end - 1] == b'/' {
        end -= 1;
    }

    // Strip trailing non-slash component
    while end > 1 && bytes[end - 1] != b'/' {
        end -= 1;
    }

    // Strip trailing slashes again
    while end > 1 && bytes[end - 1] == b'/' {
        end -= 1;
    }

    if end == 0 {
        return DOT.as_ptr() as *mut c_char;
    }

    if end == 1 && bytes[0] == b'/' {
        return SLASH.as_ptr() as *mut c_char;
    }

    // Null-terminate at end (modifies input - POSIX semantics)
    unsafe {
        *path.add(end) = 0;
    }

    path
}

// ═══════════════════════════════════════════════════════════════════════════
// Scheduling
// ═══════════════════════════════════════════════════════════════════════════

/// Yield the processor to other threads.
///
/// POSIX sched_yield — voluntarily relinquishes the CPU.
/// Wired to CLUU's sys_yield (syscall #4).
#[no_mangle]
pub extern "C" fn sched_yield() -> c_int {
    let _ = crate::syscall::yield_cpu();
    0
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

unsafe fn cstr_bytes(ptr: *const c_char) -> &'static [u8] {
    if ptr.is_null() {
        return &[];
    }
    let mut len = 0;
    let mut p = ptr;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }
    core::slice::from_raw_parts(ptr as *const u8, len)
}
