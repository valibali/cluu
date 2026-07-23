//! Marquee — scrolling text display with wrap-around separator.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Drawable, Rect};
use crate::View;

pub struct Marquee {
    text: String,
    offset: usize,
    fg: u8,
    separator: String,
    max_width: usize,
}

impl Marquee {
    pub fn new(text: &str) -> Self {
        Marquee {
            text: String::from(text),
            offset: 0,
            fg: COLOR_DEFAULT,
            separator: String::from("  ***  "),
            max_width: 0,
        }
    }

    pub fn fg(mut self, fg: u8) -> Self { self.fg = fg; self }
    pub fn separator(mut self, sep: &str) -> Self { self.separator = String::from(sep); self }

    pub fn max_width(mut self, w: usize) -> Self { self.max_width = w; self }

    pub fn set_text(&mut self, text: &str) {
        self.text = String::from(text);
        self.offset = 0;
    }

    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
    }

    pub fn tick(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        if chars.is_empty() { return; }
        let sep_len = self.separator.chars().count();
        let total = chars.len() + sep_len;
        self.offset = (self.offset + 1) % total.max(1);
    }
}

impl Drawable for Marquee {
    fn draw(&self, area: Rect, buf: &mut View) {
        let draw_width = if self.max_width > 0 {
            area.width.min(self.max_width)
        } else {
            area.width
        };
        if draw_width == 0 {
            return;
        }
        let chars: Vec<char> = self.text.chars().collect();
        if chars.is_empty() {
            for i in 0..draw_width {
                buf.set(area.y, area.x + i, Cell::new(' '));
            }
            return;
        }
        let display_len = chars.len();
        if display_len <= draw_width {
            for (i, ch) in chars.iter().enumerate() {
                buf.set(area.y, area.x + i, Cell::new(*ch).fg(self.fg));
            }
            for i in display_len..draw_width {
                buf.set(area.y, area.x + i, Cell::new(' '));
            }
            return;
        }
        let sep_chars: Vec<char> = self.separator.chars().collect();
        let total_len = display_len + sep_chars.len();
        let mut full: Vec<char> = chars.clone();
        full.extend_from_slice(&sep_chars);
        for i in 0..draw_width {
            let idx = (self.offset + i) % total_len;
            buf.set(area.y, area.x + i, Cell::new(full[idx]).fg(self.fg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Drawable;
    use alloc::vec;

    #[test]
    fn marquee_short_text_fits() {
        let m = Marquee::new("hi");
        let mut buf = View::new(10, 1);
        m.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('i'));
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn marquee_long_text_scrolls() {
        let mut m = Marquee::new("hello world");
        m.set_offset(3);
        let mut buf = View::new(5, 1);
        m.draw(Rect::new(0, 0, 5, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('l'));
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('o'));
    }

    #[test]
    fn marquee_wrap_with_separator() {
        let mut m = Marquee::new("AB");
        let mut buf = View::new(10, 1);
        m.draw(Rect::new(0, 0, 10, 1), &mut buf);
        // "AB" fits in 10, no wrapping
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('A'));

        // Now make text longer than width
        m.set_text("ABCDEFGH");
        let mut buf2 = View::new(3, 1);
        m.set_offset(8);
        m.draw(Rect::new(0, 0, 3, 1), &mut buf2);
        // After "ABCDEFGH" comes separator "  ***  "
        assert_eq!(buf2.get(0, 0).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn marquee_tick_advances() {
        let mut m = Marquee::new("hello");
        m.tick();
        assert_eq!(m.offset, 1);
    }

    #[test]
    fn marquee_empty_fills_spaces() {
        let m = Marquee::new("");
        let mut buf = View::new(5, 1);
        m.draw(Rect::new(0, 0, 5, 1), &mut buf);
        for i in 0..5 {
            assert_eq!(buf.get(0, i).map(|c| c.ch), Some(' '));
        }
    }

    #[test]
    fn marquee_fg_color() {
        let m = Marquee::new("X").fg(46);
        let mut buf = View::new(5, 1);
        m.draw(Rect::new(0, 0, 5, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.fg), Some(46));
    }

    #[test]
    fn marquee_max_width_clips() {
        let m = Marquee::new("hello world").max_width(3);
        let mut buf = View::new(10, 1);
        m.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some('l'));
        assert_eq!(buf.get(0, 3).map(|c| c.ch), Some(' '));
    }
}
