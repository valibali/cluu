//! Cell grid → SHM blit. Task 17 fills the blit body.
//!
//! Helper utilities (path building, glyph lookup) can live here from day one.

extern crate alloc;
use alloc::vec::Vec;

use crate::tty_backend::Cluuterm;

/// Blit the cell grid to the compositor SHM window.
///
/// Task 17 fills the actual pixel-writing body. For now this is a no-op so
/// the cluuterm binary compiles cleanly with the Task 15 recv loop in place.
#[allow(unused_variables)]
pub fn render(_term: &mut Cluuterm) {
    // TODO(task17): walk term.cells / term.fg_cells / term.bg_cells and blit
    // glyphs into term.shm.  After blitting send COMP_WIN_DAMAGE to
    // term.comp_ep and await COMP_FRAME_READY before returning.
}

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
