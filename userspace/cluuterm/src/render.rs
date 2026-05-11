//! Cell grid → SHM blit. Task 17 fills the blit body.
//!
//! Helper utilities (path building, glyph lookup) can live here from day one.

extern crate alloc;
use alloc::vec::Vec;

/// Build the `/dev/pts/<id>` path string at runtime.
///
/// Returns a null-terminated byte vector suitable for passing to POSIX APIs.
pub fn pts_path(id: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(24);
    buf.extend_from_slice(b"/dev/pts/");
    let mut n = id;
    let mut digits = [0u8; 10];
    let mut i = 0;
    if n == 0 {
        digits[0] = b'0';
        i = 1;
    }
    while n > 0 {
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        buf.push(digits[i]);
    }
    buf.push(0); // null-terminated for POSIX open / posix_spawn
    buf
}
