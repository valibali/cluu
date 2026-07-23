//! Cell grid primitives — the rendering surface for libtui.
//!
//! SRP split from the original lib.rs. Provides:
//! - `Cell`: a single styled character in the grid
//! - `View`: a 2D grid of cells (the renderable buffer)
//! - SGR color and attribute constants
//!
//! no_std + alloc. Pure data — no I/O, no side effects.

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

// =========================================================================
// Color constants (SGR 256-color codes, 0 = default)
// =========================================================================

/// SGR foreground/background color codes. 0 = default.
pub const COLOR_DEFAULT: u8 = 0;
pub const COLOR_BLACK: u8 = 0;
pub const COLOR_RED: u8 = 1;
pub const COLOR_GREEN: u8 = 2;
pub const COLOR_YELLOW: u8 = 3;
pub const COLOR_BLUE: u8 = 4;
pub const COLOR_MAGENTA: u8 = 5;
pub const COLOR_CYAN: u8 = 6;
pub const COLOR_WHITE: u8 = 7;

// =========================================================================
// Attribute bitflags
// =========================================================================

/// Cell attributes bitmask.
pub const ATTR_NONE: u8 = 0;
pub const ATTR_BOLD: u8 = 1;
pub const ATTR_UNDERLINE: u8 = 2;
pub const ATTR_REVERSE: u8 = 4;

// =========================================================================
// Cell
// =========================================================================

/// A single styled cell in the view grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: u8,
    pub bg: u8,
    pub attrs: u8,
}

impl Cell {
    pub fn new(ch: char) -> Self {
        Cell { ch, fg: COLOR_DEFAULT, bg: COLOR_DEFAULT, attrs: ATTR_NONE }
    }

    pub fn fg(mut self, fg: u8) -> Self {
        self.fg = fg;
        self
    }

    pub fn bg(mut self, bg: u8) -> Self {
        self.bg = bg;
        self
    }

    pub fn attrs(mut self, attrs: u8) -> Self {
        self.attrs = attrs;
        self
    }

    pub fn with_char(mut self, ch: char) -> Self {
        self.ch = ch;
        self
    }
}

// =========================================================================
// View
// =========================================================================

/// A renderable grid of cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub cells: Vec<Cell>,
    pub width: usize,
    pub height: usize,
}

impl View {
    /// Create a blank view filled with spaces.
    pub fn new(width: usize, height: usize) -> Self {
        let cells = vec![Cell::new(' '); width * height];
        View { cells, width, height }
    }

    /// Clear in-place, reusing existing allocation. Reallocates only if
    /// dimensions changed.
    pub fn reset(&mut self, width: usize, height: usize) {
        let len = width * height;
        if self.cells.len() != len {
            self.cells = vec![Cell::new(' '); len];
        } else {
            for c in &mut self.cells {
                *c = Cell::new(' ');
            }
        }
        self.width = width;
        self.height = height;
    }

    /// Get the cell at (row, col). Returns None if out of bounds.
    pub fn get(&self, row: usize, col: usize) -> Option<&Cell> {
        if row < self.height && col < self.width {
            self.cells.get(row * self.width + col)
        } else {
            None
        }
    }

    /// Set the cell at (row, col). No-op if out of bounds.
    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        if row < self.height && col < self.width {
            self.cells[row * self.width + col] = cell;
        }
    }

    /// Fill the entire view with a single cell.
    pub fn fill(&mut self, cell: Cell) {
        for c in &mut self.cells {
            *c = cell;
        }
    }

    /// Write a string at (row, col), clipping to view bounds.
    pub fn write_str(&mut self, row: usize, col: usize, s: &str) {
        let mut c = col;
        for ch in s.chars() {
            if c >= self.width || row >= self.height {
                break;
            }
            self.set(row, c, Cell::new(ch));
            c += 1;
        }
    }

    /// Write a styled string at (row, col), clipping to view bounds.
    /// Each character gets the same fg/bg/attrs.
    pub fn write_styled(&mut self, row: usize, col: usize, s: &str, cell: Cell) {
        let mut c = col;
        for ch in s.chars() {
            if c >= self.width || row >= self.height {
                break;
            }
            self.set(row, c, cell.with_char(ch));
            c += 1;
        }
    }

    /// Write a styled string at (row, col), clipping to at most `max_chars`
    /// characters AND view bounds. Use when writing inside a sub-area whose
    /// width is narrower than the View.
    pub fn write_styled_n(&mut self, row: usize, col: usize, s: &str, max_chars: usize, cell: Cell) {
        let mut c = col;
        for ch in s.chars() {
            if c >= self.width || row >= self.height || c - col >= max_chars {
                break;
            }
            self.set(row, c, cell.with_char(ch));
            c += 1;
        }
    }

    /// Write `s` truncated to `width` chars at (row, col), padding with
    /// spaces (using `cell`'s style) if shorter. No allocation.
    pub fn write_field(&mut self, row: usize, col: usize, s: &str, width: usize, cell: Cell) {
        let mut c = col;
        for ch in s.chars() {
            if c >= self.width || row >= self.height || c - col >= width {
                break;
            }
            self.set(row, c, cell.with_char(ch));
            c += 1;
        }
        while c - col < width && c < self.width && row < self.height {
            self.set(row, c, cell.with_char(' '));
            c += 1;
        }
    }

    /// Fill a rectangular region with a single cell.
    pub fn fill_rect(&mut self, row: usize, col: usize, width: usize, height: usize, cell: Cell) {
        let end_row = (row + height).min(self.height);
        let end_col = (col + width).min(self.width);
        for r in row..end_row {
            for c in col..end_col {
                self.cells[r * self.width + c] = cell;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_new_filled_with_spaces() {
        let v = View::new(3, 2);
        assert_eq!(v.width, 3);
        assert_eq!(v.height, 2);
        assert_eq!(v.cells.len(), 6);
        for cell in &v.cells {
            assert_eq!(cell.ch, ' ');
        }
    }

    #[test]
    fn view_set_and_get() {
        let mut v = View::new(3, 2);
        v.set(0, 1, Cell::new('X'));
        assert_eq!(v.get(0, 1).map(|c| c.ch), Some('X'));
        assert_eq!(v.get(0, 0).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn view_set_out_of_bounds_is_noop() {
        let mut v = View::new(2, 2);
        v.set(5, 5, Cell::new('Z'));
        assert_eq!(v.cells.len(), 4);
    }

    #[test]
    fn view_write_str_clips() {
        let mut v = View::new(3, 1);
        v.write_str(0, 0, "hello");
        assert_eq!(v.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(v.get(0, 1).map(|c| c.ch), Some('e'));
        assert_eq!(v.get(0, 2).map(|c| c.ch), Some('l'));
    }

    #[test]
    fn cell_builder_methods() {
        let c = Cell::new('A').fg(COLOR_RED).bg(COLOR_WHITE).attrs(ATTR_BOLD);
        assert_eq!(c.ch, 'A');
        assert_eq!(c.fg, COLOR_RED);
        assert_eq!(c.bg, COLOR_WHITE);
        assert_eq!(c.attrs, ATTR_BOLD);
    }

    #[test]
    fn view_fill_rect() {
        let mut v = View::new(5, 5);
        v.fill_rect(1, 1, 3, 2, Cell::new('X'));
        assert_eq!(v.get(0, 0).map(|c| c.ch), Some(' '));
        assert_eq!(v.get(1, 1).map(|c| c.ch), Some('X'));
        assert_eq!(v.get(1, 3).map(|c| c.ch), Some('X'));
        assert_eq!(v.get(2, 1).map(|c| c.ch), Some('X'));
        assert_eq!(v.get(2, 3).map(|c| c.ch), Some('X'));
        assert_eq!(v.get(3, 1).map(|c| c.ch), Some(' '));
        assert_eq!(v.get(1, 4).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn view_write_styled_n_clips_to_max() {
        let mut v = View::new(10, 1);
        v.write_styled_n(0, 0, "hello", 3, Cell::new(' ').fg(COLOR_RED));
        assert_eq!(v.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(v.get(0, 1).map(|c| c.ch), Some('e'));
        assert_eq!(v.get(0, 2).map(|c| c.ch), Some('l'));
        assert_eq!(v.get(0, 3).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn view_write_field_truncates_and_pads() {
        let mut v = View::new(10, 1);
        v.write_field(0, 0, "hi", 5, Cell::new(' ').fg(COLOR_GREEN));
        assert_eq!(v.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(v.get(0, 1).map(|c| c.ch), Some('i'));
        assert_eq!(v.get(0, 2).map(|c| c.ch), Some(' '));
        assert_eq!(v.get(0, 3).map(|c| c.ch), Some(' '));
        assert_eq!(v.get(0, 4).map(|c| c.ch), Some(' '));
        assert_eq!(v.get(0, 2).map(|c| c.fg), Some(COLOR_GREEN));
    }

    #[test]
    fn view_write_field_truncates_long() {
        let mut v = View::new(3, 1);
        v.write_field(0, 0, "hello", 3, Cell::new(' '));
        assert_eq!(v.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(v.get(0, 1).map(|c| c.ch), Some('e'));
        assert_eq!(v.get(0, 2).map(|c| c.ch), Some('l'));
    }
}
