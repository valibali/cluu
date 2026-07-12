//! Cell-buffer diff renderer — emit CSI only for changed cells.
//!
//! `ScreenBuffer` holds a grid of `Cell`s and tracks which cells were
//! modified since the last `clear_dirty()`. `diff_render` compares two
//! buffers and emits only the CSI sequences needed to update the terminal
//! from `prev` to `self`, reusing `cursor_move` and `sgr_for` from
//! `render.rs`.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::render::{cursor_move, push_char, sgr_for, RESET_SGR};
use crate::Cell;

/// A cell grid with dirty tracking for diff-based rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenBuffer {
    cells: Vec<Cell>,
    dirty: Vec<bool>,
    width: usize,
    height: usize,
}

impl ScreenBuffer {
    /// Create a buffer filled with blank cells, all marked dirty.
    pub fn new(width: usize, height: usize) -> Self {
        let len = width * height;
        ScreenBuffer {
            cells: alloc::vec![Cell::new(' '); len],
            dirty: alloc::vec![true; len],
            width,
            height,
        }
    }

    /// Set a cell at (row, col). No-op if out of bounds. Marks dirty.
    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        if row < self.height && col < self.width {
            let i = row * self.width + col;
            self.cells[i] = cell;
            self.dirty[i] = true;
        }
    }

    /// Get a reference to the cell at (row, col). None if out of bounds.
    pub fn get(&self, row: usize, col: usize) -> Option<&Cell> {
        if row < self.height && col < self.width {
            self.cells.get(row * self.width + col)
        } else {
            None
        }
    }

    /// Fill with blank cells, mark all dirty.
    pub fn clear(&mut self) {
        for c in &mut self.cells {
            *c = Cell::new(' ');
        }
        for d in &mut self.dirty {
            *d = true;
        }
    }

    /// Resize the buffer. Mark all cells dirty.
    pub fn resize(&mut self, width: usize, height: usize) {
        let len = width * height;
        self.cells = alloc::vec![Cell::new(' '); len];
        self.dirty = alloc::vec![true; len];
        self.width = width;
        self.height = height;
    }

    /// Count of dirty cells.
    pub fn dirty_count(&self) -> usize {
        self.dirty.iter().filter(|&&d| d).count()
    }

    /// Clear all dirty flags.
    pub fn clear_dirty(&mut self) {
        for d in &mut self.dirty {
            *d = false;
        }
    }

    /// Width of the buffer.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height of the buffer.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Compare self against `prev` and emit CSI only for changed cells.
    ///
    /// For each cell that differs from `prev`:
    /// - Cursor move to (row+1, col+1) — skipped if the last emitted cell
    ///   was at (row, col-1) (adjacency optimization).
    /// - SGR reset + new style if fg/bg/attrs changed.
    /// - The character itself.
    ///
    /// If the two buffers have different dimensions, all cells are emitted.
    pub fn diff_render(&self, prev: &ScreenBuffer) -> String {
        let mut out: Vec<u8> = Vec::new();
        let same_dims = self.width == prev.width && self.height == prev.height;
        let mut last_pos: Option<(usize, usize)> = None;

        for i in 0..self.cells.len() {
            let curr = self.cells[i];
            let prev_cell = if same_dims { prev.cells[i] } else { Cell::new(' ') };

            if curr == prev_cell && same_dims {
                continue;
            }

            let row = if self.width > 0 { i / self.width } else { 0 };
            let col = if self.width > 0 { i % self.width } else { 0 };

            // Skip cursor move if adjacent to last emitted cell (same row, prev col).
            let adjacent = last_pos.map_or(false, |(r, c)| r == row && c + 1 == col);
            if !adjacent {
                out.extend_from_slice(&cursor_move(row + 1, col + 1));
            }

            // SGR if style changed from prev.
            if curr.fg != prev_cell.fg
                || curr.bg != prev_cell.bg
                || curr.attrs != prev_cell.attrs
            {
                out.extend_from_slice(RESET_SGR);
                out.extend_from_slice(&sgr_for(&curr));
            }

            push_char(&mut out, curr.ch);
            last_pos = Some((row, col));
        }

        String::from_utf8(out).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{COLOR_RED, COLOR_WHITE, ATTR_BOLD};

    #[test]
    fn diff_no_changes() {
        let a = ScreenBuffer::new(3, 2);
        let b = ScreenBuffer::new(3, 2);
        let out = b.diff_render(&a);
        assert!(out.is_empty(), "identical buffers should produce empty diff");
    }

    #[test]
    fn diff_single_cell_change() {
        let prev = ScreenBuffer::new(3, 2);
        let mut curr = ScreenBuffer::new(3, 2);
        curr.set(1, 1, Cell::new('X'));
        let out = curr.diff_render(&prev);
        // Exactly 1 cursor move at (row=2, col=2) — 1-indexed.
        assert_eq!(out.matches("\x1b[2;2H").count(), 1);
        assert!(out.contains('X'));
    }

    #[test]
    fn diff_multiple_changes() {
        let prev = ScreenBuffer::new(5, 3);
        let mut curr = ScreenBuffer::new(5, 3);
        curr.set(0, 0, Cell::new('A'));
        curr.set(1, 2, Cell::new('B'));
        curr.set(2, 4, Cell::new('C'));
        let out = curr.diff_render(&prev);
        assert_eq!(out.matches("\x1b[1;1H").count(), 1);
        assert_eq!(out.matches("\x1b[2;3H").count(), 1);
        assert_eq!(out.matches("\x1b[3;5H").count(), 1);
        assert!(out.contains('A'));
        assert!(out.contains('B'));
        assert!(out.contains('C'));
    }

    #[test]
    fn diff_adjacent_cells() {
        let prev = ScreenBuffer::new(4, 1);
        let mut curr = ScreenBuffer::new(4, 1);
        curr.set(0, 0, Cell::new('A'));
        curr.set(0, 1, Cell::new('B'));
        let out = curr.diff_render(&prev);
        // First cell gets cursor move, second is adjacent — no cursor move.
        assert_eq!(out.matches("\x1b[1;1H").count(), 1);
        assert_eq!(out.matches("\x1b[1;2H").count(), 0);
        assert!(out.contains('A'));
        assert!(out.contains('B'));
    }

    #[test]
    fn diff_sgr_change_only() {
        let mut prev = ScreenBuffer::new(1, 1);
        let mut curr = ScreenBuffer::new(1, 1);
        prev.set(0, 0, Cell::new('X'));
        curr.set(0, 0, Cell::new('X').fg(COLOR_RED));
        let out = curr.diff_render(&prev);
        // SGR reset + red fg emitted because fg changed.
        assert!(out.contains("\x1b[0m"), "reset SGR should be emitted");
        assert!(out.contains("\x1b[31m"), "red fg SGR should be emitted");
        // Char re-emitted to apply new style.
        assert!(out.contains('X'));
    }

    #[test]
    fn diff_sgr_bold_and_bg_change() {
        let mut prev = ScreenBuffer::new(1, 1);
        let mut curr = ScreenBuffer::new(1, 1);
        prev.set(0, 0, Cell::new('Z'));
        curr.set(0, 0, Cell::new('Z').bg(COLOR_WHITE).attrs(ATTR_BOLD));
        let out = curr.diff_render(&prev);
        assert!(out.contains("\x1b[0m"));
        // sgr_for emits: 1 (bold); 47 (white bg)
        assert!(out.contains("\x1b[1;47m"));
    }

    #[test]
    fn diff_clear_marks_all_dirty() {
        let mut buf = ScreenBuffer::new(4, 3);
        buf.clear_dirty();
        assert_eq!(buf.dirty_count(), 0);
        buf.clear();
        assert_eq!(buf.dirty_count(), 12);
    }

    #[test]
    fn diff_resize_marks_all_dirty() {
        let mut buf = ScreenBuffer::new(2, 2);
        buf.clear_dirty();
        assert_eq!(buf.dirty_count(), 0);
        buf.resize(3, 3);
        assert_eq!(buf.dirty_count(), 9);
    }

    #[test]
    fn diff_different_dimensions_renders_all() {
        let prev = ScreenBuffer::new(2, 2);
        let curr = ScreenBuffer::new(2, 2);
        let out = curr.diff_render(&prev);
        assert!(out.is_empty());

        let prev_small = ScreenBuffer::new(1, 1);
        let out = curr.diff_render(&prev_small);
        assert!(out.contains("\x1b[1;1H"));
        assert!(out.contains("\x1b[2;1H"));
        assert_eq!(out.matches(' ').count(), 4);
    }

    #[test]
    fn diff_set_and_get() {
        let mut buf = ScreenBuffer::new(3, 2);
        buf.set(0, 1, Cell::new('Q'));
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('Q'));
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
        assert_eq!(buf.get(5, 5), None);
    }

    #[test]
    fn diff_set_marks_dirty() {
        let mut buf = ScreenBuffer::new(3, 2);
        buf.clear_dirty();
        assert_eq!(buf.dirty_count(), 0);
        buf.set(0, 1, Cell::new('X'));
        assert_eq!(buf.dirty_count(), 1);
    }

    #[test]
    fn diff_clear_dirty_resets_count() {
        let mut buf = ScreenBuffer::new(2, 2);
        assert_eq!(buf.dirty_count(), 4);
        buf.clear_dirty();
        assert_eq!(buf.dirty_count(), 0);
    }

    #[test]
    fn diff_adjacency_breaks_on_gap() {
        let prev = ScreenBuffer::new(5, 1);
        let mut curr = ScreenBuffer::new(5, 1);
        curr.set(0, 0, Cell::new('A'));
        curr.set(0, 2, Cell::new('C'));
        let out = curr.diff_render(&prev);
        // Both cells need cursor moves — they're not adjacent (gap at col 1).
        assert_eq!(out.matches("\x1b[1;1H").count(), 1);
        assert_eq!(out.matches("\x1b[1;3H").count(), 1);
    }
}
