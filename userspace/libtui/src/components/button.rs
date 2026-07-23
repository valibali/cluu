//! Button — labeled button with active/focused states and bracket style.

extern crate alloc;

use alloc::string::String;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD};
use crate::layout::{Drawable, Rect};
use crate::View;

pub struct Button {
    label: String,
    active: bool,
    focused: bool,
    fg: u8,
    active_fg: u8,
    focused_fg: u8,
    bracket_fg: u8,
    use_brackets: bool,
}

impl Button {
    pub fn new(label: &str) -> Self {
        Button {
            label: String::from(label),
            active: false,
            focused: false,
            fg: 252,
            active_fg: 255,
            focused_fg: 226,
            bracket_fg: 244,
            use_brackets: true,
        }
    }

    pub fn active(mut self, active: bool) -> Self { self.active = active; self }
    pub fn focused(mut self, focused: bool) -> Self { self.focused = focused; self }
    pub fn fg(mut self, fg: u8) -> Self { self.fg = fg; self }
    pub fn active_fg(mut self, fg: u8) -> Self { self.active_fg = fg; self }
    pub fn focused_fg(mut self, fg: u8) -> Self { self.focused_fg = fg; self }
    pub fn bracket_fg(mut self, fg: u8) -> Self { self.bracket_fg = fg; self }
    pub fn brackets(mut self, use_brackets: bool) -> Self { self.use_brackets = use_brackets; self }

    pub fn set_active(&mut self, active: bool) { self.active = active; }
    pub fn set_focused(&mut self, focused: bool) { self.focused = focused; }
    pub fn set_label(&mut self, label: &str) { self.label = String::from(label); }

    pub fn width(&self) -> usize {
        let label_len = self.label.chars().count();
        if self.use_brackets { label_len + 2 } else { label_len }
    }
}

impl Drawable for Button {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let fg = if self.focused {
            self.focused_fg
        } else if self.active {
            self.active_fg
        } else {
            self.fg
        };

        let mut col = area.x;

        if self.use_brackets {
            let bracket = if self.focused { '[' } else { ' ' };
            let bracket_fg = if self.focused { self.focused_fg } else { self.bracket_fg };
            buf.set(area.y, col, Cell::new(bracket).fg(bracket_fg));
            col += 1;
        }

        for ch in self.label.chars() {
            if col >= area.x + area.width { break; }
            buf.set(area.y, col, Cell::new(ch).fg(fg).attrs(ATTR_BOLD));
            col += 1;
        }

        if self.use_brackets {
            let bracket = if self.focused { ']' } else { ' ' };
            let bracket_fg = if self.focused { self.focused_fg } else { self.bracket_fg };
            if col < area.x + area.width {
                buf.set(area.y, col, Cell::new(bracket).fg(bracket_fg));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Drawable;

    #[test]
    fn button_basic_draw() {
        let b = Button::new("OK");
        let mut buf = View::new(10, 1);
        b.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('O'));
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some('K'));
        assert_eq!(buf.get(0, 3).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn button_focused_shows_brackets() {
        let b = Button::new("OK").focused(true);
        let mut buf = View::new(10, 1);
        b.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('['));
        assert_eq!(buf.get(0, 3).map(|c| c.ch), Some(']'));
        assert_eq!(buf.get(0, 1).map(|c| c.fg), Some(226));
    }

    #[test]
    fn button_active_color() {
        let b = Button::new("Play").active(true);
        let mut buf = View::new(10, 1);
        b.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 1).map(|c| c.fg), Some(255));
    }

    #[test]
    fn button_no_brackets() {
        let b = Button::new("OK").brackets(false);
        let mut buf = View::new(10, 1);
        b.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('O'));
    }

    #[test]
    fn button_width() {
        assert_eq!(Button::new("OK").width(), 4);
        assert_eq!(Button::new("OK").brackets(false).width(), 2);
    }
}
