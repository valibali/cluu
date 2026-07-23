//! Scrollable content pane — holds lines, offset, max_visible.
//!
//! A viewport displays a window of lines from a larger content set,
//! supporting scroll up/down and scroll-to-line operations.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::View;

/// A scrollable pane of text lines.
pub struct Viewport {
    lines: Vec<String>,
    offset: usize,
    max_visible: usize,
}

impl Viewport {
    /// Create an empty viewport that can show `max_visible` lines at once.
    pub fn new(max_visible: usize) -> Self {
        Viewport {
            lines: Vec::new(),
            offset: 0,
            max_visible,
        }
    }

    /// Replace all content and reset offset to 0.
    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = lines;
        self.offset = 0;
    }

    /// Decrease offset by `n`, clamped at 0.
    pub fn scroll_up(&mut self, n: usize) {
        self.offset = self.offset.saturating_sub(n);
    }

    /// Increase offset by `n`, clamped at `lines.len() - max_visible`.
    pub fn scroll_down(&mut self, n: usize) {
        let max = self.lines.len().saturating_sub(self.max_visible);
        self.offset = (self.offset + n).min(max);
    }

    /// Set offset so that `line` is visible. If `line` is already visible,
    /// no change. If below the window, scrolls so `line` is at the bottom.
    /// If above the window, scrolls so `line` is at the top.
    pub fn scroll_to(&mut self, line: usize) {
        if self.max_visible == 0 {
            self.offset = 0;
            return;
        }
        if line < self.offset {
            self.offset = line;
        } else if line >= self.offset + self.max_visible {
            self.offset = line.saturating_add(1).saturating_sub(self.max_visible);
        }
        let max = self.lines.len().saturating_sub(self.max_visible);
        if self.offset > max {
            self.offset = max;
        }
    }

    /// Return the slice of currently visible lines.
    pub fn visible_lines(&self) -> &[String] {
        let end = (self.offset + self.max_visible).min(self.lines.len());
        &self.lines[self.offset..end]
    }

    /// Write visible lines into a `View` starting at `(row, col)`.
    pub fn render(&self, row: usize, col: usize, view: &mut View) {
        let visible = self.visible_lines();
        for (i, line) in visible.iter().enumerate() {
            let r = row + i;
            if r >= view.height {
                break;
            }
            view.write_str(r, col, line);
        }
    }

    /// Total number of lines stored.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Current scroll offset (index of first visible line).
    pub fn offset(&self) -> usize {
        self.offset
    }
}

impl crate::layout::Drawable for Viewport {
    fn draw(&self, area: crate::layout::Rect, buf: &mut View) {
        let visible = self.visible_lines();
        for (i, line) in visible.iter().enumerate() {
            if i >= area.height {
                break;
            }
            buf.write_str(area.y + i, area.x, line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    fn make_lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line{}", i)).collect()
    }

    #[test]
    fn viewport_new_is_empty() {
        let vp = Viewport::new(5);
        assert_eq!(vp.line_count(), 0);
        assert_eq!(vp.offset(), 0);
        assert!(vp.visible_lines().is_empty());
    }

    #[test]
    fn viewport_set_lines_resets_offset() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        assert_eq!(vp.offset(), 0);
        assert_eq!(vp.line_count(), 10);
        vp.scroll_down(3);
        assert_eq!(vp.offset(), 3);
        vp.set_lines(make_lines(5));
        assert_eq!(vp.offset(), 0);
    }

    #[test]
    fn viewport_scroll_up_clamps_at_zero() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        vp.scroll_down(2);
        assert_eq!(vp.offset(), 2);
        vp.scroll_up(5);
        assert_eq!(vp.offset(), 0);
    }

    #[test]
    fn viewport_scroll_down_clamps_at_max() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        vp.scroll_down(100);
        // max = 10 - 3 = 7
        assert_eq!(vp.offset(), 7);
    }

    #[test]
    fn viewport_scroll_down_basic() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        vp.scroll_down(2);
        assert_eq!(vp.offset(), 2);
        vp.scroll_down(3);
        assert_eq!(vp.offset(), 5);
    }

    #[test]
    fn viewport_scroll_to_already_visible() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        vp.scroll_down(2);
        // visible range [2, 5), line 3 is visible
        vp.scroll_to(3);
        assert_eq!(vp.offset(), 2);
    }

    #[test]
    fn viewport_scroll_to_below_window() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        // offset 0, visible [0, 3), scroll to 5
        vp.scroll_to(5);
        // offset = 5 + 1 - 3 = 3, visible [3, 6), 5 is visible
        assert_eq!(vp.offset(), 3);
    }

    #[test]
    fn viewport_scroll_to_above_window() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        vp.scroll_down(5);
        // offset 5, visible [5, 8), scroll to 2
        vp.scroll_to(2);
        assert_eq!(vp.offset(), 2);
    }

    #[test]
    fn viewport_scroll_to_clamps_at_max() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        vp.scroll_to(9);
        // offset = 9 + 1 - 3 = 7, max = 7
        assert_eq!(vp.offset(), 7);
    }

    #[test]
    fn viewport_visible_lines_slice() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        vp.scroll_down(2);
        let vis = vp.visible_lines();
        assert_eq!(vis.len(), 3);
        assert_eq!(vis[0], "line2");
        assert_eq!(vis[1], "line3");
        assert_eq!(vis[2], "line4");
    }

    #[test]
    fn viewport_visible_lines_at_end() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        vp.scroll_down(100);
        let vis = vp.visible_lines();
        // offset 7, visible [7, 10) = 3 lines
        assert_eq!(vis.len(), 3);
        assert_eq!(vis[0], "line7");
        assert_eq!(vis[2], "line9");
    }

    #[test]
    fn viewport_render_writes_lines() {
        let mut vp = Viewport::new(2);
        vp.set_lines(make_lines(5));
        vp.scroll_down(1);
        // visible: line1, line2
        let mut view = View::new(10, 5);
        vp.render(1, 2, &mut view);
        assert_eq!(view.get(1, 2).map(|c| c.ch), Some('l'));
        assert_eq!(view.get(1, 3).map(|c| c.ch), Some('i'));
        assert_eq!(view.get(2, 2).map(|c| c.ch), Some('l'));
        // row 0 should still be blank
        assert_eq!(view.get(0, 2).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn viewport_render_clips_to_view_height() {
        let mut vp = Viewport::new(5);
        vp.set_lines(make_lines(5));
        let mut view = View::new(10, 2);
        // only 2 rows available starting at row 0
        vp.render(0, 0, &mut view);
        assert_eq!(view.get(0, 0).map(|c| c.ch), Some('l'));
        assert_eq!(view.get(1, 0).map(|c| c.ch), Some('l'));
        // row 2 doesn't exist (height=2), should be None
        assert!(view.get(2, 0).is_none());
    }

    #[test]
    fn viewport_fewer_lines_than_max_visible() {
        let mut vp = Viewport::new(10);
        vp.set_lines(make_lines(3));
        vp.scroll_down(5);
        // max = 3 - 10 = 0 (saturating), offset stays 0
        assert_eq!(vp.offset(), 0);
        assert_eq!(vp.visible_lines().len(), 3);
    }

    #[test]
    fn viewport_scroll_to_zero() {
        let mut vp = Viewport::new(3);
        vp.set_lines(make_lines(10));
        vp.scroll_down(5);
        vp.scroll_to(0);
        assert_eq!(vp.offset(), 0);
    }
}
