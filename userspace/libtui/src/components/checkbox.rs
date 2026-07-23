//! Checkbox — toggle with label.

extern crate alloc;

use alloc::string::String;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD};
use crate::layout::{Drawable, Rect};
use crate::View;

pub struct Checkbox {
    checked: bool,
    label: String,
    fg: u8,
    check_fg: u8,
}

impl Checkbox {
    pub fn new(label: &str) -> Self {
        Checkbox {
            checked: false,
            label: String::from(label),
            fg: COLOR_DEFAULT,
            check_fg: COLOR_DEFAULT,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn fg(mut self, fg: u8) -> Self { self.fg = fg; self }
    pub fn check_fg(mut self, fg: u8) -> Self { self.check_fg = fg; self }

    pub fn toggle(&mut self) {
        self.checked = !self.checked;
    }

    pub fn set_checked(&mut self, checked: bool) {
        self.checked = checked;
    }

    pub fn is_checked(&self) -> bool {
        self.checked
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

impl Drawable for Checkbox {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let bracket = if self.checked { "[✓] " } else { "[ ] " };
        let bracket_len = bracket.chars().count();
        buf.write_styled_n(area.y, area.x, bracket, area.width, Cell::new(' ').fg(self.check_fg).attrs(ATTR_BOLD));

        let label_max = area.width.saturating_sub(bracket_len);
        buf.write_styled_n(area.y, area.x + bracket_len, &self.label, label_max, Cell::new(' ').fg(self.fg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkbox_new_unchecked() {
        let c = Checkbox::new("Enable");
        assert!(!c.is_checked());
    }

    #[test]
    fn checkbox_toggle() {
        let mut c = Checkbox::new("X");
        c.toggle();
        assert!(c.is_checked());
        c.toggle();
        assert!(!c.is_checked());
    }

    #[test]
    fn checkbox_set_checked() {
        let mut c = Checkbox::new("X");
        c.set_checked(true);
        assert!(c.is_checked());
    }

    #[test]
    fn checkbox_draw_unchecked() {
        let c = Checkbox::new("Save");
        let mut buf = View::new(10, 1);
        c.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('['));
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some(' '));
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some(']'));
    }

    #[test]
    fn checkbox_draw_checked() {
        let c = Checkbox::new("Save").checked(true);
        let mut buf = View::new(10, 1);
        c.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('✓'));
    }

    #[test]
    fn checkbox_draw_label() {
        let c = Checkbox::new("Save");
        let mut buf = View::new(10, 1);
        c.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('S'));
        assert_eq!(buf.get(0, 7).map(|c| c.ch), Some('e'));
    }
}
