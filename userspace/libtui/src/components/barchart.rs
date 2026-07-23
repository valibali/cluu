//! BarChart — vertical bar chart from data series.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Drawable, Rect};
use crate::View;

const BAR_CHARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub struct BarEntry {
    pub label: String,
    pub value: u64,
}

impl BarEntry {
    pub fn new(label: &str, value: u64) -> Self {
        BarEntry { label: String::from(label), value }
    }
}

pub struct BarChart {
    entries: Vec<BarEntry>,
    fg: u8,
    label_fg: u8,
    max_bars: usize,
    auto_scale: bool,
    fixed_max: u64,
}

impl BarChart {
    pub fn new() -> Self {
        BarChart {
            entries: Vec::new(),
            fg: COLOR_DEFAULT,
            label_fg: 8,
            max_bars: 20,
            auto_scale: true,
            fixed_max: 0,
        }
    }

    pub fn entry(mut self, label: &str, value: u64) -> Self {
        self.entries.push(BarEntry::new(label, value));
        self
    }

    pub fn set_entries(&mut self, entries: Vec<BarEntry>) {
        self.entries = entries;
    }

    pub fn fg(mut self, fg: u8) -> Self { self.fg = fg; self }
    pub fn label_fg(mut self, fg: u8) -> Self { self.label_fg = fg; self }
    pub fn max_bars(mut self, n: usize) -> Self { self.max_bars = n; self }
    pub fn fixed_max(mut self, max: u64) -> Self { self.fixed_max = max; self.auto_scale = false; self }

    fn current_max(&self) -> u64 {
        if self.auto_scale {
            self.entries.iter().map(|e| e.value).max().unwrap_or(1).max(1)
        } else {
            self.fixed_max.max(1)
        }
    }
}

impl Default for BarChart {
    fn default() -> Self {
        BarChart::new()
    }
}

impl Drawable for BarChart {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 || self.entries.is_empty() {
            return;
        }

        let label_height = 1;
        let bar_height = area.height.saturating_sub(label_height);
        if bar_height == 0 {
            return;
        }

        let bar_count = self.entries.len().min(self.max_bars).min(area.width);
        let bar_width = area.width / bar_count.max(1);
        let max = self.current_max();

        for (i, entry) in self.entries.iter().take(bar_count).enumerate() {
            let bar_x = area.x + i * bar_width;
            let full_blocks = if max == 0 { 0 } else {
                (entry.value * bar_height as u64 / max) as usize
            };
            let remainder = if max == 0 { 0 } else {
                ((entry.value * bar_height as u64 * 8) / max) as usize % 8
            };

            for row in 0..bar_height {
                let y = area.y + bar_height - 1 - row;
                let ch = if row < full_blocks {
                    '█'
                } else if row == full_blocks && remainder > 0 {
                    BAR_CHARS[remainder - 1]
                } else {
                    continue;
                };
                for dx in 0..bar_width {
                    if bar_x + dx < area.x + area.width {
                        buf.set(y, bar_x + dx, Cell::new(ch).fg(self.fg));
                    }
                }
            }

            let label_y = area.y + bar_height;
            let label_clip = bar_width.min((area.x + area.width).saturating_sub(bar_x));
            buf.write_styled_n(label_y, bar_x, &entry.label, label_clip, Cell::new(' ').fg(self.label_fg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barchart_empty_noop() {
        let bc = BarChart::new();
        let mut buf = View::new(10, 5);
        bc.draw(Rect::new(0, 0, 10, 5), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn barchart_draws_bars() {
        let bc = BarChart::new().entry("A", 5).entry("B", 10);
        let mut buf = View::new(10, 5);
        bc.draw(Rect::new(0, 0, 10, 5), &mut buf);
        assert_eq!(buf.get(3, 0).map(|c| c.ch), Some('█'));
        assert_eq!(buf.get(0, 5).map(|c| c.ch), Some('█'));
        assert_eq!(buf.get(4, 0).map(|c| c.ch), Some('A'));
        assert_eq!(buf.get(4, 5).map(|c| c.ch), Some('B'));
    }

    #[test]
    fn barchart_auto_scale() {
        let bc = BarChart::new().entry("X", 50).entry("Y", 25);
        let mut buf = View::new(10, 5);
        bc.draw(Rect::new(0, 0, 10, 5), &mut buf);
        assert!(buf.get(0, 0).is_some());
    }

    #[test]
    fn barchart_fixed_max() {
        let bc = BarChart::new().fixed_max(1000).entry("X", 100);
        let mut buf = View::new(10, 5);
        bc.draw(Rect::new(0, 0, 10, 5), &mut buf);
        // 100/1000 = 10% of 4 bar rows = 0.4 → partial block at bottom
        assert_ne!(buf.get(3, 0).map(|c| c.ch), Some(' '));
    }
}
