//! CSI escape sequence generation + stdout rendering.
//!
//! Pure CSI generation functions return `Vec<u8>` and are testable without
//! a TTY. `Renderer` writes bytes to stdout (fd 1) via `libcluu::posix::_write`.

extern crate alloc;
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use crate::{Cell, View, COLOR_DEFAULT};

// --- CSI byte constants ---

/// ESC byte.
pub const ESC: u8 = 0x1B;

/// Enter alternate screen: `CSI ?1049h`
pub const ENTER_ALT_SCREEN: &[u8] = b"\x1b[?1049h";
/// Exit alternate screen: `CSI ?1049l`
pub const EXIT_ALT_SCREEN: &[u8] = b"\x1b[?1049l";
/// Clear entire screen: `CSI 2J`
pub const CLEAR_SCREEN: &[u8] = b"\x1b[2J";
/// Cursor home: `CSI H`
pub const CURSOR_HOME: &[u8] = b"\x1b[H";
/// Reset all SGR attributes: `CSI 0m`
pub const RESET_SGR: &[u8] = b"\x1b[0m";

// --- Pure CSI generation (testable) ---

/// Move cursor to (row, col) — 1-indexed. Produces `CSI row;col H`.
pub fn cursor_move(row: usize, col: usize) -> Vec<u8> {
    format!("\x1b[{};{}H", row, col).into_bytes()
}

/// Set SGR foreground color. 0 = default fg (`CSI 39 m`); all other
/// values use 256-color (`CSI 38;5;N m`).
pub fn sgr_fg(fg: u8) -> Vec<u8> {
    if fg == COLOR_DEFAULT {
        b"\x1b[39m".to_vec()
    } else {
        format!("\x1b[38;5;{}m", fg).into_bytes()
    }
}

/// Set SGR background color. 0 = default bg (`CSI 49 m`); all other
/// values use 256-color (`CSI 48;5;N m`).
pub fn sgr_bg(bg: u8) -> Vec<u8> {
    if bg == COLOR_DEFAULT {
        b"\x1b[49m".to_vec()
    } else {
        format!("\x1b[48;5;{}m", bg).into_bytes()
    }
}

/// Build SGR sequence for a cell's fg, bg, and attrs.
pub fn sgr_for(cell: &Cell) -> Vec<u8> {
    let mut parts: Vec<u8> = Vec::new();
    if cell.attrs & crate::ATTR_BOLD != 0 {
        parts.extend_from_slice(b"1;");
    }
    if cell.attrs & crate::ATTR_UNDERLINE != 0 {
        parts.extend_from_slice(b"4;");
    }
    if cell.attrs & crate::ATTR_REVERSE != 0 {
        parts.extend_from_slice(b"7;");
    }
    if cell.fg != COLOR_DEFAULT {
        parts.extend_from_slice(format!("38;5;{};", cell.fg).as_bytes());
    }
    if cell.bg != COLOR_DEFAULT {
        parts.extend_from_slice(format!("48;5;{};", cell.bg).as_bytes());
    }
    if parts.is_empty() {
        return RESET_SGR.to_vec();
    }
    if parts.last() == Some(&b';') {
        parts.pop();
    }
    let mut out = vec![ESC, b'['];
    out.extend_from_slice(&parts);
    out.push(b'm');
    out
}

/// Render a `View` to a CSI byte stream.
///
/// For v0 this does a full-screen redraw: home cursor, then per-row
/// clear-line + cell output with SGR transitions. Diff-based rendering
/// is a future task.
pub fn render_view(view: &View) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 * 1024);
    out.extend_from_slice(CURSOR_HOME);
    let mut prev: Option<Cell> = None;
    for row in 0..view.height {
        out.extend_from_slice(&cursor_move(row + 1, 1));
        out.extend_from_slice(b"\x1b[K"); // clear line
        for col in 0..view.width {
            let cell = view.cells[row * view.width + col];
            if prev.map_or(true, |p| p.fg != cell.fg || p.bg != cell.bg || p.attrs != cell.attrs) {
                out.extend_from_slice(&sgr_for(&cell));
                prev = Some(cell);
            }
            push_char(&mut out, cell.ch);
        }
    }
    out.extend_from_slice(RESET_SGR);
    out
}

pub fn push_char(out: &mut Vec<u8>, ch: char) {
    // Encode char as UTF-8 (handles non-ASCII; ASCII is a single byte).
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    out.extend_from_slice(s.as_bytes());
}

// --- Renderer (writes to stdout fd 1) ---

/// Writes CSI byte sequences to stdout (fd 1) via `libcluu::posix::_write`.
#[cfg(feature = "runtime")]
pub struct Renderer;

#[cfg(feature = "runtime")]
impl Renderer {
    pub fn new() -> Self {
        Renderer
    }

    /// Write raw bytes to stdout, looping until all are written.
    /// Short writes happen because the VFS path chunks large buffers;
    /// dropping the tail silently corrupts the terminal state.
    pub fn write(&self, bytes: &[u8]) {
        let mut sent = 0usize;
        while sent < bytes.len() {
            let n = libcluu::posix::_write(
                1,
                bytes[sent..].as_ptr() as *const core::ffi::c_void,
                bytes.len() - sent,
            );
            if n <= 0 {
                return;
            }
            sent += n as usize;
        }
    }

    pub fn enter_alt_screen(&self) {
        self.write(ENTER_ALT_SCREEN);
    }

    pub fn exit_alt_screen(&self) {
        self.write(EXIT_ALT_SCREEN);
    }

    pub fn clear_screen(&self) {
        self.write(CLEAR_SCREEN);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, View, COLOR_RED, COLOR_WHITE};

    #[test]
    fn cursor_move_format() {
        let bytes = cursor_move(5, 10);
        assert_eq!(bytes, b"\x1b[5;10H");
    }

    #[test]
    fn cursor_move_1_1() {
        let bytes = cursor_move(1, 1);
        assert_eq!(bytes, b"\x1b[1;1H");
    }

    #[test]
    fn sgr_fg_red() {
        let bytes = sgr_fg(COLOR_RED);
        assert_eq!(bytes, b"\x1b[38;5;1m");
    }

    #[test]
    fn sgr_fg_default() {
        let bytes = sgr_fg(COLOR_DEFAULT);
        assert_eq!(bytes, b"\x1b[39m");
    }

    #[test]
    fn sgr_bg_white() {
        let bytes = sgr_bg(COLOR_WHITE);
        assert_eq!(bytes, b"\x1b[48;5;7m");
    }

    #[test]
    fn sgr_for_default_cell_is_reset() {
        let cell = Cell::new('X');
        let bytes = sgr_for(&cell);
        assert_eq!(bytes, RESET_SGR);
    }

    #[test]
    fn sgr_for_bold_red_fg() {
        let cell = Cell::new('X').fg(COLOR_RED).attrs(crate::ATTR_BOLD);
        let bytes = sgr_for(&cell);
        assert_eq!(bytes, b"\x1b[1;38;5;1m");
    }

    #[test]
    fn sgr_for_fg_and_bg() {
        let cell = Cell::new('X').fg(COLOR_RED).bg(COLOR_WHITE);
        let bytes = sgr_for(&cell);
        assert_eq!(bytes, b"\x1b[38;5;1;48;5;7m");
    }

    #[test]
    fn render_view_starts_with_home() {
        let view = View::new(2, 1);
        let bytes = render_view(&view);
        assert!(bytes.starts_with(CURSOR_HOME));
    }

    #[test]
    fn render_view_ends_with_reset() {
        let view = View::new(2, 1);
        let bytes = render_view(&view);
        assert!(bytes.ends_with(RESET_SGR));
    }

    #[test]
    fn render_view_contains_clear_line() {
        let view = View::new(2, 1);
        let bytes = render_view(&view);
        assert!(bytes.windows(3).any(|w| w == b"\x1b[K"));
    }

    #[test]
    fn render_view_with_styled_cell() {
        let mut view = View::new(1, 1);
        view.set(0, 0, Cell::new('H').fg(COLOR_RED));
        let bytes = render_view(&view);
        assert!(bytes.windows(9).any(|w| w == b"\x1b[38;5;1m"));
        assert!(bytes.contains(&b'H'));
    }
}
