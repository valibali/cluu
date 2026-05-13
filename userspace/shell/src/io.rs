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
