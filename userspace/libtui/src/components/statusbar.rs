//! StatusBar — bottom bar with left and right segments.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD};
use crate::layout::{Drawable, Rect};
use crate::View;

pub struct StatusBar {
    left: Vec<String>,
    right: Vec<String>,
    fg: u8,
    bg: u8,
    bold: bool,
}

impl StatusBar {
    pub fn new() -> Self {
        StatusBar {
            left: Vec::new(),
            right: Vec::new(),
            fg: COLOR_DEFAULT,
            bg: COLOR_DEFAULT,
            bold: false,
        }
    }

    pub fn left_segment(mut self, text: &str) -> Self {
        self.left.push(String::from(text));
        self
    }

    pub fn right_segment(mut self, text: &str) -> Self {
        self.right.push(String::from(text));
        self
    }

    pub fn add_left(&mut self, text: &str) {
        self.left.push(String::from(text));
    }

    pub fn add_right(&mut self, text: &str) {
        self.right.push(String::from(text));
    }

    pub fn fg(mut self, fg: u8) -> Self { self.fg = fg; self }
    pub fn bg(mut self, bg: u8) -> Self { self.bg = bg; self }
    pub fn bold(mut self) -> Self { self.bold = true; self }
}

impl Default for StatusBar {
    fn default() -> Self {
        StatusBar::new()
    }
}

impl Drawable for StatusBar {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let mut attrs = 0u8;
        if self.bold { attrs |= ATTR_BOLD; }

        if self.bg != COLOR_DEFAULT {
            buf.fill_rect(area.y, area.x, area.width, area.height, Cell::new(' ').bg(self.bg));
        }

        let mut cell = Cell::new(' ').attrs(attrs);
        if self.fg != COLOR_DEFAULT { cell = cell.fg(self.fg); }
        if self.bg != COLOR_DEFAULT { cell = cell.bg(self.bg); }

        let mut x = area.x;
        for (i, seg) in self.left.iter().enumerate() {
            if i > 0 {
                if x < area.x + area.width {
                    buf.set(area.y, x, cell);
                    x += 1;
                }
            }
            let rem = (area.x + area.width).saturating_sub(x);
            buf.write_styled_n(area.y, x, seg, rem, cell);
            x += seg.chars().count().min(rem);
        }

        let right_text: String = {
            let mut s = String::new();
            for (i, seg) in self.right.iter().enumerate() {
                if i > 0 { s.push(' '); }
                s.push_str(seg);
            }
            s
        };
        let right_len = right_text.chars().count();
        if right_len > 0 && right_len < area.width {
            let right_x = area.x + area.width - right_len;
            if right_x >= x {
                buf.write_styled(area.y, right_x, &right_text, cell);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statusbar_left_segments() {
        let sb = StatusBar::new().left_segment("READY").left_segment("ln:42");
        let mut buf = View::new(30, 1);
        sb.draw(Rect::new(0, 0, 30, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('R'));
        assert_eq!(buf.get(0, 5).map(|c| c.ch), Some(' '));
        assert_eq!(buf.get(0, 6).map(|c| c.ch), Some('l'));
    }

    #[test]
    fn statusbar_right_segments() {
        let sb = StatusBar::new().right_segment("UTF-8").right_segment("80x24");
        let mut buf = View::new(30, 1);
        sb.draw(Rect::new(0, 0, 30, 1), &mut buf);
        let right_text = "UTF-8 80x24";
        let start = 30 - right_text.chars().count();
        assert_eq!(buf.get(0, start).map(|c| c.ch), Some('U'));
        assert_eq!(buf.get(0, start + 6).map(|c| c.ch), Some('8'));
        assert_eq!(buf.get(0, 29).map(|c| c.ch), Some('4'));
    }

    #[test]
    fn statusbar_bg_fill() {
        let sb = StatusBar::new().bg(4).left_segment("X");
        let mut buf = View::new(5, 1);
        sb.draw(Rect::new(0, 0, 5, 1), &mut buf);
        for x in 0..5 {
            assert_eq!(buf.get(0, x).map(|c| c.bg), Some(4));
        }
    }

    #[test]
    fn statusbar_bold() {
        let sb = StatusBar::new().bold().left_segment("X");
        let mut buf = View::new(5, 1);
        sb.draw(Rect::new(0, 0, 5, 1), &mut buf);
        assert!(buf.get(0, 0).unwrap().attrs & ATTR_BOLD != 0);
    }
}
