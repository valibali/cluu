//! HelpLine — keybinding display for app footers.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD};
use crate::layout::{Drawable, Rect};
use crate::View;

pub struct HelpEntry {
    pub key: String,
    pub desc: String,
}

impl HelpEntry {
    pub fn new(key: &str, desc: &str) -> Self {
        HelpEntry { key: String::from(key), desc: String::from(desc) }
    }
}

pub struct HelpLine {
    entries: Vec<HelpEntry>,
    key_fg: u8,
    desc_fg: u8,
    sep: String,
}

impl HelpLine {
    pub fn new() -> Self {
        HelpLine {
            entries: Vec::new(),
            key_fg: COLOR_DEFAULT,
            desc_fg: 8,
            sep: String::from("  "),
        }
    }

    pub fn entry(mut self, key: &str, desc: &str) -> Self {
        self.entries.push(HelpEntry::new(key, desc));
        self
    }

    pub fn key_fg(mut self, fg: u8) -> Self { self.key_fg = fg; self }
    pub fn desc_fg(mut self, fg: u8) -> Self { self.desc_fg = fg; self }
    pub fn separator(mut self, sep: &str) -> Self { self.sep = String::from(sep); self }

    pub fn add(&mut self, key: &str, desc: &str) {
        self.entries.push(HelpEntry::new(key, desc));
    }
}

impl Default for HelpLine {
    fn default() -> Self {
        HelpLine::new()
    }
}

impl Drawable for HelpLine {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mut x = area.x;
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                for ch in self.sep.chars() {
                    if x >= area.x + area.width { return; }
                    buf.set(area.y, x, Cell::new(ch).fg(self.desc_fg));
                    x += 1;
                }
            }
            for ch in entry.key.chars() {
                if x >= area.x + area.width { return; }
                buf.set(area.y, x, Cell::new(ch).fg(self.key_fg).attrs(ATTR_BOLD));
                x += 1;
            }
            buf.set(area.y, x, Cell::new(' ').fg(self.desc_fg));
            x += 1;
            for ch in entry.desc.chars() {
                if x >= area.x + area.width { return; }
                buf.set(area.y, x, Cell::new(ch).fg(self.desc_fg));
                x += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpline_draws_key_and_desc() {
        let h = HelpLine::new().entry("q", "quit").entry("j", "down");
        let mut buf = View::new(30, 1);
        h.draw(Rect::new(0, 0, 30, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('q'));
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some(' '));
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some('q'));
        assert_eq!(buf.get(0, 3).map(|c| c.ch), Some('u'));
        assert_eq!(buf.get(0, 6).map(|c| c.ch), Some(' '));
        assert_eq!(buf.get(0, 8).map(|c| c.ch), Some('j'));
    }

    #[test]
    fn helpline_key_is_bold() {
        let h = HelpLine::new().entry("q", "quit");
        let mut buf = View::new(10, 1);
        h.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert!(buf.get(0, 0).unwrap().attrs & ATTR_BOLD != 0);
    }

    #[test]
    fn helpline_clips_to_width() {
        let h = HelpLine::new().entry("Ctrl+Shift+X", "do something big");
        let mut buf = View::new(5, 1);
        h.draw(Rect::new(0, 0, 5, 1), &mut buf);
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('+'));
    }

    #[test]
    fn helpline_add_method() {
        let mut h = HelpLine::new();
        h.add("a", "first");
        h.add("b", "second");
        let mut buf = View::new(30, 1);
        h.draw(Rect::new(0, 0, 30, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('a'));
    }
}
