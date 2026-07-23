//! Sparkline — inline mini-chart for time-series data (last N samples).

extern crate alloc;

use alloc::vec::Vec;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Drawable, Rect};
use crate::View;

const BLOCK_CHARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub struct Sparkline {
    data: Vec<u64>,
    max_samples: usize,
    fg: u8,
    auto_scale: bool,
    fixed_max: u64,
}

impl Sparkline {
    pub fn new(max_samples: usize) -> Self {
        Sparkline {
            data: Vec::new(),
            max_samples,
            fg: COLOR_DEFAULT,
            auto_scale: true,
            fixed_max: 0,
        }
    }

    pub fn fg(mut self, fg: u8) -> Self {
        self.fg = fg;
        self
    }

    pub fn auto_scale(mut self, auto_scale: bool) -> Self {
        self.auto_scale = auto_scale;
        self
    }

    pub fn fixed_max(mut self, max: u64) -> Self {
        self.fixed_max = max;
        self.auto_scale = false;
        self
    }

    pub fn push(&mut self, value: u64) {
        if self.data.len() >= self.max_samples {
            self.data.remove(0);
        }
        self.data.push(value);
    }

    pub fn set_data(&mut self, data: Vec<u64>) {
        self.data = data;
        if self.data.len() > self.max_samples {
            self.data = self.data.split_off(self.data.len() - self.max_samples);
        }
    }

    pub fn clear(&mut self) {
        self.data.clear();
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    fn current_max(&self) -> u64 {
        if self.auto_scale {
            self.data.iter().copied().max().unwrap_or(1).max(1)
        } else {
            self.fixed_max.max(1)
        }
    }

    fn block_char(value: u64, max: u64) -> char {
        if value == 0 || max == 0 {
            return '▁';
        }
        let level = (value.saturating_mul(BLOCK_CHARS.len() as u64) - 1) / max;
        let idx = (level as usize).min(BLOCK_CHARS.len() - 1);
        BLOCK_CHARS[idx]
    }
}

impl Drawable for Sparkline {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let max = self.current_max();
        let start = if self.data.len() > area.width {
            self.data.len() - area.width
        } else {
            0
        };

        for (i, &value) in self.data[start..].iter().enumerate() {
            if i >= area.width {
                break;
            }
            let ch = Self::block_char(value, max);
            buf.set(area.y, area.x + i, Cell::new(ch).fg(self.fg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparkline_new_is_empty() {
        let s = Sparkline::new(10);
        assert!(s.is_empty());
    }

    #[test]
    fn sparkline_push_grows() {
        let mut s = Sparkline::new(10);
        s.push(5);
        s.push(10);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn sparkline_evicts_old() {
        let mut s = Sparkline::new(3);
        s.push(1); s.push(2); s.push(3); s.push(4);
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn sparkline_clear() {
        let mut s = Sparkline::new(10);
        s.push(1); s.push(2);
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn sparkline_block_char_zero() {
        assert_eq!(Sparkline::block_char(0, 100), '▁');
    }

    #[test]
    fn sparkline_block_char_max() {
        assert_eq!(Sparkline::block_char(100, 100), '█');
    }

    #[test]
    fn sparkline_block_char_mid() {
        let ch = Sparkline::block_char(50, 100);
        assert!(ch >= '▃' && ch <= '▅');
    }

    #[test]
    fn sparkline_draw_writes_chars() {
        let mut s = Sparkline::new(10);
        s.push(10); s.push(5); s.push(0);
        let mut buf = View::new(10, 1);
        s.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('█'));
        assert_ne!(buf.get(0, 1).map(|c| c.ch), Some(' '));
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some('▁'));
    }

    #[test]
    fn sparkline_fixed_max() {
        let mut s = Sparkline::new(10).fixed_max(1000);
        s.push(500);
        assert_eq!(s.current_max(), 1000);
    }

    #[test]
    fn sparkline_set_data_truncates() {
        let mut s = Sparkline::new(3);
        s.set_data(alloc::vec![1, 2, 3, 4, 5]);
        assert_eq!(s.len(), 3);
    }
}
