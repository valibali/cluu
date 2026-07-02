use alloc::format;
use libcluu::debug_print;

/// Write bytes to POSIX fd 1 (stdout). Used by all prompt/output writes.
///
/// Goes through VFS → PTS → terminal (cluuterm) in the new architecture.
/// Legacy tty-container path is gone (autologin ripped).
pub fn write_stdout(bytes: &[u8]) {
    extern "C" {
        fn _write(fd: i32, buf: *const u8, n: usize) -> isize;
    }
    if !bytes.is_empty() {
        let _ = unsafe { _write(1, bytes.as_ptr(), bytes.len()) };
    }
}

/// Write bytes to POSIX fd 2 (stderr).
pub fn write_stderr(bytes: &[u8]) {
    extern "C" {
        fn _write(fd: i32, buf: *const u8, n: usize) -> isize;
    }
    if !bytes.is_empty() {
        let _ = unsafe { _write(2, bytes.as_ptr(), bytes.len()) };
    }
}

/// Log a silently-dropped error to debug serial + stderr.
pub fn report_err<T, E: core::fmt::Debug>(result: Result<T, E>, context: &str) {
    if let Err(e) = result {
        let msg = format!("shell: {}: {:?}\n", context, e);
        let _ = debug_print(&msg);
        write_stderr(msg.as_bytes());
    }
}
