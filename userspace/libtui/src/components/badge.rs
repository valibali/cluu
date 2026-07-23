//! Badge — short colored status label.

extern crate alloc;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD};
use crate::layout::{Drawable, Rect};
use crate::View;

pub struct Badge {
    text: alloc::string::String,
    fg: u8,
    bg: u8,
    bold: bool,
}

impl Badge {
    pub fn new(text: &str) -> Self {
        Badge {
            text: alloc::string::String::from(text),
            fg: COLOR_DEFAULT,
            bg: COLOR_DEFAULT,
            bold: false,
        }
    }

    pub fn fg(mut self, fg: u8) -> Self { self.fg = fg; self }
    pub fn bg(mut self, bg: u8) -> Self { self.bg = bg; self }
    pub fn bold(mut self) -> Self { self.bold = true; self }

    pub fn set_text(&mut self, text: &str) {
        self.text = alloc::string::String::from(text);
    }

    pub fn width(&self) -> usize {
        self.text.chars().count()
    }
}

impl Drawable for Badge {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mut attrs = 0u8;
        if self.bold { attrs |= ATTR_BOLD; }

        for (i, ch) in self.text.chars().enumerate() {
            if i >= area.width {
                break;
            }
            let mut cell = Cell::new(ch).attrs(attrs);
            if self.fg != COLOR_DEFAULT { cell = cell.fg(self.fg); }
            if self.bg != COLOR_DEFAULT { cell = cell.bg(self.bg); }
            buf.set(area.y, area.x + i, cell);
        }
        for i in self.text.chars().count()..area.width {
            let mut cell = Cell::new(' ');
            if self.bg != COLOR_DEFAULT { cell = cell.bg(self.bg); }
            buf.set(area.y, area.x + i, cell);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_draws_text() {
        let b = Badge::new("OK");
        let mut buf = View::new(5, 1);
        b.draw(Rect::new(0, 0, 5, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('O'));
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('K'));
    }

    #[test]
    fn badge_fills_bg() {
        let b = Badge::new("OK").bg(2);
        let mut buf = View::new(5, 1);
        b.draw(Rect::new(0, 0, 5, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.bg), Some(2));
        assert_eq!(buf.get(0, 4).map(|c| c.bg), Some(2));
    }

    #[test]
    fn badge_bold() {
        let b = Badge::new("X").bold();
        let mut buf = View::new(1, 1);
        b.draw(Rect::new(0, 0, 1, 1), &mut buf);
        assert!(buf.get(0, 0).unwrap().attrs & ATTR_BOLD != 0);
    }

    #[test]
    fn badge_width() {
        assert_eq!(Badge::new("hello").width(), 5);
    }
}
