//! Scrollbar — vertical scrollbar with proportional thumb.

extern crate alloc;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Drawable, Rect};
use crate::View;

pub struct Scrollbar {
    total: usize,
    visible: usize,
    offset: usize,
    thumb_fg: u8,
    track_fg: u8,
    thumb_char: char,
    track_char: char,
}

impl Scrollbar {
    pub fn new(total: usize, visible: usize) -> Self {
        Scrollbar {
            total,
            visible,
            offset: 0,
            thumb_fg: 240,
            track_fg: 238,
            thumb_char: '█',
            track_char: '│',
        }
    }

    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
    }

    pub fn set_total(&mut self, total: usize) {
        self.total = total;
    }

    pub fn thumb_fg(mut self, fg: u8) -> Self { self.thumb_fg = fg; self }
    pub fn track_fg(mut self, fg: u8) -> Self { self.track_fg = fg; self }
    pub fn thumb_char(mut self, ch: char) -> Self { self.thumb_char = ch; self }
    pub fn track_char(mut self, ch: char) -> Self { self.track_char = ch; self }
}

impl Drawable for Scrollbar {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.height == 0 || self.total == 0 {
            return;
        }
        let thumb_size = ((self.visible * area.height) / self.total).max(1);
        let max_offset = self.total.saturating_sub(self.visible);
        let thumb_start = if max_offset == 0 {
            0
        } else {
            (self.offset * (area.height.saturating_sub(thumb_size))) / max_offset
        };
        for i in 0..area.height {
            let is_thumb = i >= thumb_start && i < thumb_start + thumb_size;
            let ch = if is_thumb { self.thumb_char } else { self.track_char };
            let fg = if is_thumb { self.thumb_fg } else { self.track_fg };
            buf.set(area.y + i, area.x, Cell::new(ch).fg(fg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Drawable;
    use alloc::vec::Vec;

    #[test]
    fn scrollbar_thumb_position() {
        let sb = Scrollbar::new(100, 10).offset(50);
        let mut buf = View::new(1, 10);
        sb.draw(Rect::new(0, 0, 1, 10), &mut buf);
        let thumb_cells: Vec<_> = (0..10).filter(|&r| buf.get(r, 0).map(|c| c.ch == '█').unwrap_or(false)).collect();
        assert!(!thumb_cells.is_empty());
    }

    #[test]
    fn scrollbar_all_visible() {
        let sb = Scrollbar::new(5, 10);
        let mut buf = View::new(1, 5);
        sb.draw(Rect::new(0, 0, 1, 5), &mut buf);
        for r in 0..5 {
            assert_eq!(buf.get(r, 0).map(|c| c.ch), Some('█'));
        }
    }

    #[test]
    fn scrollbar_zero_total_noop() {
        let sb = Scrollbar::new(0, 10);
        let mut buf = View::new(1, 5);
        sb.draw(Rect::new(0, 0, 1, 5), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn scrollbar_custom_chars() {
        let sb = Scrollbar::new(100, 10).offset(0).thumb_char('#').track_char('.');
        let mut buf = View::new(1, 10);
        sb.draw(Rect::new(0, 0, 1, 10), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('#'));
        assert_eq!(buf.get(9, 0).map(|c| c.ch), Some('.'));
    }
}
