//! Divider — horizontal or vertical separator line with style.

extern crate alloc;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Drawable, Rect};
use crate::View;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DividerDirection {
    #[default]
    Horizontal,
    Vertical,
}

pub struct Divider {
    direction: DividerDirection,
    char: char,
    fg: u8,
}

impl Divider {
    pub fn horizontal() -> Self {
        Divider { direction: DividerDirection::Horizontal, char: '─', fg: COLOR_DEFAULT }
    }

    pub fn vertical() -> Self {
        Divider { direction: DividerDirection::Vertical, char: '│', fg: COLOR_DEFAULT }
    }

    pub fn char(mut self, ch: char) -> Self {
        self.char = ch;
        self
    }

    pub fn fg(mut self, fg: u8) -> Self {
        self.fg = fg;
        self
    }
}

impl Drawable for Divider {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        match self.direction {
            DividerDirection::Horizontal => {
                let y = area.y;
                for x in area.x..area.x + area.width {
                    buf.set(y, x, Cell::new(self.char).fg(self.fg));
                }
            }
            DividerDirection::Vertical => {
                let x = area.x;
                for y in area.y..area.y + area.height {
                    buf.set(y, x, Cell::new(self.char).fg(self.fg));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divider_horizontal_draws_line() {
        let d = Divider::horizontal();
        let mut buf = View::new(5, 3);
        d.draw(Rect::new(0, 1, 5, 1), &mut buf);
        for x in 0..5 {
            assert_eq!(buf.get(1, x).map(|c| c.ch), Some('─'));
        }
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn divider_vertical_draws_line() {
        let d = Divider::vertical();
        let mut buf = View::new(3, 5);
        d.draw(Rect::new(1, 0, 1, 5), &mut buf);
        for y in 0..5 {
            assert_eq!(buf.get(y, 1).map(|c| c.ch), Some('│'));
        }
    }

    #[test]
    fn divider_custom_char() {
        let d = Divider::horizontal().char('=');
        let mut buf = View::new(3, 1);
        d.draw(Rect::new(0, 0, 3, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('='));
    }

    #[test]
    fn divider_fg() {
        let d = Divider::horizontal().fg(5);
        let mut buf = View::new(3, 1);
        d.draw(Rect::new(0, 0, 3, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.fg), Some(5));
    }
}
