//! Progress bar component — a horizontal bar showing percentage completion.
//!
//! Pure state: holds percent (0..=100), width, and style. Renders into a
//! View via `Drawable`. No I/O, no_std + alloc.

extern crate alloc;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Drawable, Rect};
use crate::View;

pub struct Progress {
    percent: u8,
    width: usize,
    full_fg: u8,
    empty_fg: u8,
    show_pct: bool,
}

impl Progress {
    pub fn new(width: usize) -> Self {
        Progress {
            percent: 0,
            width,
            full_fg: COLOR_DEFAULT,
            empty_fg: COLOR_DEFAULT,
            show_pct: true,
        }
    }

    pub fn with_percent(mut self, p: u8) -> Self {
        self.percent = p.min(100);
        self
    }

    pub fn full_fg(mut self, fg: u8) -> Self {
        self.full_fg = fg;
        self
    }

    pub fn empty_fg(mut self, fg: u8) -> Self {
        self.empty_fg = fg;
        self
    }

    pub fn show_percentage(mut self, show: bool) -> Self {
        self.show_pct = show;
        self
    }

    pub fn set_percent(&mut self, p: u8) {
        self.percent = p.min(100);
    }

    pub fn percent(&self) -> u8 {
        self.percent
    }
}

impl Drawable for Progress {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let bar_width = if self.show_pct && area.width > 4 {
            area.width - 4
        } else {
            area.width
        };

        let filled = if bar_width > 0 {
            (self.percent as usize * bar_width) / 100
        } else {
            0
        };

        for i in 0..bar_width {
            let ch = if i < filled { '█' } else { '░' };
            let fg = if i < filled { self.full_fg } else { self.empty_fg };
            buf.set(area.y, area.x + i, Cell::new(ch).fg(fg));
        }

        if self.show_pct && area.width > 4 {
            let pct_str: alloc::string::String = alloc::format!("{}%", self.percent);
            let start = area.x + bar_width;
            for (i, ch) in pct_str.chars().enumerate() {
                if start + i >= area.x + area.width {
                    break;
                }
                buf.set(area.y, start + i, Cell::new(ch));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_new_is_zero() {
        let p = Progress::new(20);
        assert_eq!(p.percent(), 0);
    }

    #[test]
    fn progress_clamps_above_100() {
        let p = Progress::new(20).with_percent(150);
        assert_eq!(p.percent(), 100);
    }

    #[test]
    fn progress_set_percent_clamps() {
        let mut p = Progress::new(20);
        p.set_percent(200);
        assert_eq!(p.percent(), 100);
    }

    #[test]
    fn progress_draw_zero_percent() {
        let p = Progress::new(10).show_percentage(false);
        let mut buf = View::new(10, 1);
        p.draw(Rect::new(0, 0, 10, 1), &mut buf);
        for i in 0..10 {
            assert_eq!(buf.get(0, i).map(|c| c.ch), Some('░'));
        }
    }

    #[test]
    fn progress_draw_full_percent() {
        let p = Progress::new(10).with_percent(100).show_percentage(false);
        let mut buf = View::new(10, 1);
        p.draw(Rect::new(0, 0, 10, 1), &mut buf);
        for i in 0..10 {
            assert_eq!(buf.get(0, i).map(|c| c.ch), Some('█'));
        }
    }

    #[test]
    fn progress_draw_half_percent() {
        let p = Progress::new(10).with_percent(50).show_percentage(false);
        let mut buf = View::new(10, 1);
        p.draw(Rect::new(0, 0, 10, 1), &mut buf);
        for i in 0..5 {
            assert_eq!(buf.get(0, i).map(|c| c.ch), Some('█'));
        }
        for i in 5..10 {
            assert_eq!(buf.get(0, i).map(|c| c.ch), Some('░'));
        }
    }

    #[test]
    fn progress_draw_with_percentage() {
        let p = Progress::new(10).with_percent(52);
        let mut buf = View::new(10, 1);
        p.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 6).map(|c| c.ch), Some('5'));
        assert_eq!(buf.get(0, 7).map(|c| c.ch), Some('2'));
        assert_eq!(buf.get(0, 8).map(|c| c.ch), Some('%'));
    }

    #[test]
    fn progress_draw_with_offset() {
        let p = Progress::new(5).with_percent(50).show_percentage(false);
        let mut buf = View::new(10, 3);
        p.draw(Rect::new(2, 1, 5, 1), &mut buf);
        assert_eq!(buf.get(1, 2).map(|c| c.ch), Some('█'));
        assert_eq!(buf.get(1, 4).map(|c| c.ch), Some('░'));
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn progress_draw_zero_width_noop() {
        let p = Progress::new(10).with_percent(50);
        let mut buf = View::new(10, 1);
        p.draw(Rect::zero(), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn progress_draw_colored() {
        let p = Progress::new(10)
            .with_percent(50)
            .full_fg(2)
            .empty_fg(8)
            .show_percentage(false);
        let mut buf = View::new(10, 1);
        p.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.fg), Some(2));
        assert_eq!(buf.get(0, 5).map(|c| c.fg), Some(8));
    }
}
