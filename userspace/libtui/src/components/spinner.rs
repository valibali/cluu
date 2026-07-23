//! Spinner — animated loading indicator with selectable frame sets.

extern crate alloc;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Drawable, Rect};
use crate::View;

#[derive(Debug, Clone, Copy)]
pub enum SpinnerStyle {
    Dots,
    Line,
    Arc,
    Arrow,
    BouncingBar,
}

impl SpinnerStyle {
    pub fn frames(self) -> &'static [char] {
        match self {
            SpinnerStyle::Dots => &['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'],
            SpinnerStyle::Line => &['-', '\\', '|', '/'],
            SpinnerStyle::Arc => &['◜', '◠', '◝', '◞', '◡', '◟'],
            SpinnerStyle::Arrow => &['←', '↖', '↑', '↗', '→', '↘', '↓', '↙'],
            SpinnerStyle::BouncingBar => &['[', '=', ']', ' ', ' ', '[', '=', ']'],
        }
    }
}

pub struct Spinner {
    frame: usize,
    style: SpinnerStyle,
    fg: u8,
}

impl Spinner {
    pub fn new(style: SpinnerStyle) -> Self {
        Spinner { frame: 0, style, fg: COLOR_DEFAULT }
    }

    pub fn fg(mut self, fg: u8) -> Self {
        self.fg = fg;
        self
    }

    pub fn tick(&mut self) {
        let len = self.style.frames().len();
        self.frame = (self.frame + 1) % len;
    }

    pub fn current_char(&self) -> char {
        self.style.frames()[self.frame]
    }

    pub fn reset(&mut self) {
        self.frame = 0;
    }
}

impl Drawable for Spinner {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        buf.set(area.y, area.x, Cell::new(self.current_char()).fg(self.fg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_new_starts_at_zero() {
        let s = Spinner::new(SpinnerStyle::Line);
        assert_eq!(s.current_char(), '-');
    }

    #[test]
    fn spinner_tick_advances() {
        let mut s = Spinner::new(SpinnerStyle::Line);
        s.tick();
        assert_eq!(s.current_char(), '\\');
        s.tick();
        assert_eq!(s.current_char(), '|');
    }

    #[test]
    fn spinner_wraps_around() {
        let mut s = Spinner::new(SpinnerStyle::Line);
        for _ in 0..4 {
            s.tick();
        }
        assert_eq!(s.current_char(), '-');
    }

    #[test]
    fn spinner_reset() {
        let mut s = Spinner::new(SpinnerStyle::Dots);
        s.tick(); s.tick(); s.tick();
        s.reset();
        assert_eq!(s.current_char(), '⣾');
    }

    #[test]
    fn spinner_draw_writes_char() {
        let s = Spinner::new(SpinnerStyle::Line).fg(3);
        let mut buf = View::new(5, 1);
        s.draw(Rect::new(0, 0, 1, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('-'));
        assert_eq!(buf.get(0, 0).map(|c| c.fg), Some(3));
    }

    #[test]
    fn spinner_dots_has_8_frames() {
        assert_eq!(SpinnerStyle::Dots.frames().len(), 8);
    }
}
