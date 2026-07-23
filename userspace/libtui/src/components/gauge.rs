//! Gauge — value bar with min/max range and optional label.

extern crate alloc;

use alloc::string::String;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Drawable, Rect};
use crate::View;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GaugeDirection {
    #[default]
    Horizontal,
    Vertical,
}

const BLOCK_CHARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub struct Gauge {
    value: u64,
    max: u64,
    fg: u8,
    bg: u8,
    label: Option<String>,
    show_bar: bool,
    direction: GaugeDirection,
    track_char: char,
}

impl Gauge {
    pub fn new(max: u64) -> Self {
        Gauge { value: 0, max, fg: COLOR_DEFAULT, bg: COLOR_DEFAULT, label: None, show_bar: true, direction: GaugeDirection::Horizontal, track_char: '░' }
    }

    pub fn value(mut self, v: u64) -> Self {
        self.value = v.min(self.max);
        self
    }

    pub fn set_value(&mut self, v: u64) {
        self.value = v.min(self.max);
    }

    pub fn fg(mut self, fg: u8) -> Self {
        self.fg = fg;
        self
    }

    pub fn bg(mut self, bg: u8) -> Self {
        self.bg = bg;
        self
    }

    pub fn label(mut self, label: &str) -> Self {
        self.label = Some(String::from(label));
        self
    }

    pub fn show_bar(mut self, show: bool) -> Self {
        self.show_bar = show;
        self
    }

    pub fn vertical(mut self) -> Self {
        self.direction = GaugeDirection::Vertical;
        self
    }

    pub fn direction(mut self, dir: GaugeDirection) -> Self {
        self.direction = dir;
        self
    }

    pub fn track_char(mut self, ch: char) -> Self {
        self.track_char = ch;
        self
    }

    pub fn percent(&self) -> u8 {
        if self.max == 0 {
            return 0;
        }
        ((self.value * 100) / self.max) as u8
    }
}

impl Drawable for Gauge {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        match self.direction {
            GaugeDirection::Horizontal => self.draw_horizontal(area, buf),
            GaugeDirection::Vertical => self.draw_vertical(area, buf),
        }
    }
}

impl Gauge {
    fn draw_horizontal(&self, area: Rect, buf: &mut View) {
        let bar_width = if self.show_bar { area.width } else { 0 };
        let filled = if self.max == 0 || bar_width == 0 {
            0
        } else {
            ((self.value * bar_width as u64) / self.max) as usize
        };

        if self.show_bar {
            for i in 0..bar_width {
                let ch = if i < filled { '█' } else { self.track_char };
                let mut cell = Cell::new(ch);
                if i < filled { cell = cell.fg(self.fg); }
                else { cell = cell.fg(self.bg); }
                buf.set(area.y, area.x + i, cell);
            }
        }

        if let Some(ref label) = self.label {
            let label_chars: alloc::vec::Vec<char> = label.chars().collect();
            let label_x = if filled + 1 < bar_width.saturating_sub(label_chars.len()) {
                area.x + filled + 1
            } else {
                area.x
            };
            for (i, ch) in label_chars.iter().enumerate() {
                if label_x + i >= area.x + area.width {
                    break;
                }
                buf.set(area.y, label_x + i, Cell::new(*ch));
            }
        }
    }

    fn draw_vertical(&self, area: Rect, buf: &mut View) {
        if !self.show_bar || self.max == 0 {
            return;
        }

        let total_eighths = (self.value * (area.height as u64 * 8)) / self.max.max(1);
        let total_eighths = total_eighths as usize;
        let max_eighths = area.height * 8;

        for row in 0..area.height {
            let from_bottom = area.height - 1 - row;
            let cell_start = from_bottom * 8;
            let cell_end = cell_start + 8;

            let fill = if total_eighths >= cell_end {
                8
            } else if total_eighths > cell_start {
                total_eighths - cell_start
            } else {
                0
            };

            let ch = if fill >= 8 {
                '█'
            } else if fill > 0 {
                BLOCK_CHARS[fill]
            } else {
                self.track_char
            };

            let cell_fg = if fill > 0 { self.fg } else { self.bg };
            for col in 0..area.width {
                buf.set(area.y + row, area.x + col, Cell::new(ch).fg(cell_fg));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauge_new_is_zero() {
        let g = Gauge::new(100);
        assert_eq!(g.percent(), 0);
    }

    #[test]
    fn gauge_value_clamps() {
        let g = Gauge::new(50).value(100);
        assert_eq!(g.percent(), 100);
    }

    #[test]
    fn gauge_half() {
        let g = Gauge::new(100).value(50);
        assert_eq!(g.percent(), 50);
    }

    #[test]
    fn gauge_zero_max() {
        let g = Gauge::new(0).value(10);
        assert_eq!(g.percent(), 0);
    }

    #[test]
    fn gauge_draw_half_filled() {
        let g = Gauge::new(100).value(50);
        let mut buf = View::new(10, 1);
        g.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('█'));
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('█'));
        assert_eq!(buf.get(0, 5).map(|c| c.ch), Some('░'));
        assert_eq!(buf.get(0, 9).map(|c| c.ch), Some('░'));
    }

    #[test]
    fn gauge_draw_label() {
        let g = Gauge::new(100).value(30).label("30%");
        let mut buf = View::new(10, 1);
        g.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('3'));
        assert_eq!(buf.get(0, 5).map(|c| c.ch), Some('0'));
        assert_eq!(buf.get(0, 6).map(|c| c.ch), Some('%'));
    }

    #[test]
    fn gauge_no_bar_label_only() {
        let g = Gauge::new(100).value(50).show_bar(false).label("CPU 50%");
        let mut buf = View::new(10, 1);
        g.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('C'));
        assert_eq!(buf.get(0, 3).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn gauge_vertical_half() {
        let g = Gauge::new(100).value(50).vertical().fg(46);
        let mut buf = View::new(1, 4);
        g.draw(Rect::new(0, 0, 1, 4), &mut buf);
        assert_eq!(buf.get(3, 0).map(|c| c.ch), Some('█'));
        assert_eq!(buf.get(2, 0).map(|c| c.ch), Some('█'));
        assert_eq!(buf.get(1, 0).map(|c| c.ch), Some('░'));
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('░'));
    }

    #[test]
    fn gauge_vertical_full() {
        let g = Gauge::new(100).value(100).vertical().fg(46);
        let mut buf = View::new(1, 3);
        g.draw(Rect::new(0, 0, 1, 3), &mut buf);
        for r in 0..3 {
            assert_eq!(buf.get(r, 0).map(|c| c.ch), Some('█'));
        }
    }

    #[test]
    fn gauge_vertical_partial_block() {
        let g = Gauge::new(100).value(10).vertical().fg(46);
        let mut buf = View::new(1, 4);
        g.draw(Rect::new(0, 0, 1, 4), &mut buf);
        // 10% of 4*8=32 eighths = 3 → '▄' (BLOCK_CHARS[3])
        assert_eq!(buf.get(3, 0).map(|c| c.ch), Some('▄'));
        assert_eq!(buf.get(2, 0).map(|c| c.ch), Some('░'));
    }

    #[test]
    fn gauge_vertical_track_char() {
        let g = Gauge::new(100).value(0).vertical().track_char('·').fg(46);
        let mut buf = View::new(1, 3);
        g.draw(Rect::new(0, 0, 1, 3), &mut buf);
        for r in 0..3 {
            assert_eq!(buf.get(r, 0).map(|c| c.ch), Some('·'));
        }
    }
}
