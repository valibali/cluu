//! BigText — large block-character digit display (Winamp-style 3x3 glyphs).
//!
//! Renders numbers using 3-row block characters. Supports optional sign,
//! colon separator, and configurable digit count. Extracted from cluuamp's
//! `draw_block_time` / `BLOCK_DIGITS` for reuse across CLUU apps.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Drawable, Rect};
use crate::View;

/// 3x3 block-digit glyphs for digits 0-9.
pub const BLOCK_DIGITS: [[&str; 3]; 10] = [
    ["█▀█", "█ █", "█▄█"], // 0
    [" █ ", " █ ", "▄█▄"], // 1
    ["█▀█", "▄▀▀", "█▄█"], // 2
    ["█▀█", " ▀█", "█▄█"], // 3
    ["█ █", "▀▀█", "  █"], // 4
    ["█▀▀", "▀▀█", "▄▄█"], // 5
    ["█▀▀", "█▀█", "█▄█"], // 6
    ["▀▀█", "  █", "  █"], // 7
    ["█▀█", "█▀█", "█▄█"], // 8
    ["█▀█", "▀▀█", "▄▄█"], // 9
];

/// Minus sign glyph (2 cols wide, 3 rows tall).
pub const BLOCK_MINUS: [&str; 3] = ["  ", "▀▀", "  "];

/// Colon glyph (1 col wide, 3 rows tall).
pub const BLOCK_COLON: [&str; 3] = ["▄", " ", "▀"];

/// Digit width in characters (each digit glyph is 3 cols wide).
pub const DIGIT_WIDTH: usize = 3;

/// Spacing between digit groups.
pub const GROUP_SPACING: usize = 1;

pub struct BigText {
    digits: Vec<u8>,
    negative: bool,
    show_colon: bool,
    colon_position: Option<usize>,
    fg: u8,
    bg: u8,
    spacing: usize,
}

impl BigText {
    pub fn new() -> Self {
        BigText {
            digits: Vec::new(),
            negative: false,
            show_colon: false,
            colon_position: None,
            fg: COLOR_DEFAULT,
            bg: COLOR_DEFAULT,
            spacing: 1,
        }
    }

    /// Set the number to display. Extracts individual digits.
    pub fn value(mut self, n: u64) -> Self {
        self.digits = if n == 0 {
            alloc::vec![0]
        } else {
            let mut v = Vec::new();
            let mut n = n;
            while n > 0 {
                v.push((n % 10) as u8);
                n /= 10;
            }
            v.reverse();
            v
        };
        self
    }

    /// Set digits directly (e.g. for fixed-width displays like mm:ss).
    pub fn digits(mut self, digits: &[u8]) -> Self {
        self.digits = digits.iter().map(|&d| d.min(9)).collect();
        self
    }

    pub fn negative(mut self, neg: bool) -> Self {
        self.negative = neg;
        self
    }

    pub fn fg(mut self, fg: u8) -> Self {
        self.fg = fg;
        self
    }

    pub fn bg(mut self, bg: u8) -> Self {
        self.bg = bg;
        self
    }

    pub fn spacing(mut self, n: usize) -> Self {
        self.spacing = n;
        self
    }

    /// Insert a colon separator after the digit at `position` (0-indexed
    /// from the left). E.g. for `-mm:ss`, colon after digit index 1.
    pub fn colon_after(mut self, position: usize) -> Self {
        self.show_colon = true;
        self.colon_position = Some(position);
        self
    }

    /// Set a time value as mm:ss. Renders as 4 digits with colon between.
    pub fn time(self, mins: u64, secs: u64) -> Self {
        self.digits(&[
            ((mins / 10) % 10) as u8,
            (mins % 10) as u8,
            ((secs / 10) % 10) as u8,
            (secs % 10) as u8,
        ])
        .colon_after(1)
    }

    /// Total width needed to render: sign + digits + colon + spacing.
    pub fn width(&self) -> usize {
        let sign_w = if self.negative { 2 + self.spacing } else { 0 };
        let digits_w = self.digits.len() * DIGIT_WIDTH;
        let digit_spacing = self.digits.len().saturating_sub(1) * self.spacing;
        let colon_w = if self.show_colon { 1 } else { 0 };
        let group_spacing = if self.show_colon { self.spacing * 2 } else { 0 };
        sign_w + digits_w + digit_spacing + colon_w + group_spacing
    }

    /// Height is always 3 rows.
    pub const HEIGHT: usize = 3;
}

impl Default for BigText {
    fn default() -> Self {
        BigText::new()
    }
}

impl Drawable for BigText {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height < 3 {
            return;
        }

        if self.bg != COLOR_DEFAULT {
            buf.fill_rect(area.y, area.x, area.width, area.height, Cell::new(' ').bg(self.bg));
        }

        let mut col = area.x;

        if self.negative && col + 2 <= area.x + area.width {
            for row in 0..3 {
                for (i, ch) in BLOCK_MINUS[row].chars().enumerate() {
                    if col + i < area.x + area.width {
                        buf.set(area.y + row, col + i, Cell::new(ch).fg(self.fg).bg(self.bg));
                    }
                }
            }
            col += 2 + self.spacing;
        }

        for (di, &digit) in self.digits.iter().enumerate() {
            if di > 0 {
                col += self.spacing;
            }

            if self.show_colon {
                if let Some(cp) = self.colon_position {
                    if di == cp + 1 {
                        col += self.spacing;
                        if col + 1 <= area.x + area.width {
                            for row in 0..3 {
                                for (i, ch) in BLOCK_COLON[row].chars().enumerate() {
                                    if col + i < area.x + area.width {
                                        buf.set(area.y + row, col + i, Cell::new(ch).fg(self.fg).bg(self.bg));
                                    }
                                }
                            }
                        }
                        col += 1 + self.spacing;
                    }
                }
            }

            if col + DIGIT_WIDTH > area.x + area.width {
                break;
            }

            let glyph = &BLOCK_DIGITS[digit as usize];
            for row in 0..3 {
                for (i, ch) in glyph[row].chars().enumerate() {
                    if col + i < area.x + area.width {
                        buf.set(area.y + row, col + i, Cell::new(ch).fg(self.fg).bg(self.bg));
                    }
                }
            }
            col += DIGIT_WIDTH;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigtext_value_zero() {
        let bt = BigText::new().value(0);
        assert_eq!(bt.digits, alloc::vec![0]);
    }

    #[test]
    fn bigtext_value_extracts_digits() {
        let bt = BigText::new().value(123);
        assert_eq!(bt.digits, alloc::vec![1, 2, 3]);
    }

    #[test]
    fn bigtext_time_format() {
        let bt = BigText::new().time(12, 34);
        assert_eq!(bt.digits, alloc::vec![1, 2, 3, 4]);
        assert!(bt.show_colon);
        assert_eq!(bt.colon_position, Some(1));
    }

    #[test]
    fn bigtext_width_no_sign_no_colon() {
        let bt = BigText::new().digits(&[1, 2, 3]);
        // 3 digits * 3 cols + 2 gaps * 1 spacing = 11
        assert_eq!(bt.width(), 11);
    }

    #[test]
    fn bigtext_width_with_sign() {
        let bt = BigText::new().digits(&[1, 2]).negative(true);
        // sign(2) + spacing(1) + 2 digits(6) + 1 gap(1) = 10
        assert_eq!(bt.width(), 10);
    }

    #[test]
    fn bigtext_width_time_format() {
        let bt = BigText::new().time(12, 34);
        // 4 digits(12) + 3 gaps(3) + 2 group gaps(2) + colon(1) = 18
        assert_eq!(bt.width(), 18);
    }

    #[test]
    fn bigtext_width_zero_spacing() {
        let bt = BigText::new().digits(&[1, 2, 3]).spacing(0);
        assert_eq!(bt.width(), 9);
    }

    #[test]
    fn bigtext_draw_digit() {
        let bt = BigText::new().digits(&[8]);
        let mut buf = View::new(3, 3);
        bt.draw(Rect::new(0, 0, 3, 3), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('█'));
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('▀'));
        assert_eq!(buf.get(2, 1).map(|c| c.ch), Some('▄'));
    }

    #[test]
    fn bigtext_draw_negative_sign() {
        let bt = BigText::new().digits(&[0]).negative(true);
        let mut buf = View::new(10, 3);
        bt.draw(Rect::new(0, 0, 10, 3), &mut buf);
        assert_eq!(buf.get(1, 0).map(|c| c.ch), Some('▀'));
        assert_eq!(buf.get(1, 1).map(|c| c.ch), Some('▀'));
    }

    #[test]
    fn bigtext_draw_time_with_colon() {
        let bt = BigText::new().time(12, 34);
        let w = bt.width();
        let mut buf = View::new(w, 3);
        bt.draw(Rect::new(0, 0, w, 3), &mut buf);
        // Digit '1' at cols 0-2
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('█'));
        // Digit '2' at cols 4-6 (after 1-col gap)
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('█'));
        // Gap at 8, colon at 9
        assert_eq!(buf.get(0, 9).map(|c| c.ch), Some('▄'));
        assert_eq!(buf.get(2, 9).map(|c| c.ch), Some('▀'));
        // Gap at 10, digit '3' at cols 11-13
        assert_eq!(buf.get(0, 11).map(|c| c.ch), Some('█'));
        // Digit '4' at cols 15-17
        assert_eq!(buf.get(0, 15).map(|c| c.ch), Some('█'));
    }

    #[test]
    fn bigtext_draw_clips_to_width() {
        let bt = BigText::new().digits(&[1, 2, 3]);
        let mut buf = View::new(5, 3);
        bt.draw(Rect::new(0, 0, 5, 3), &mut buf);
        // Only first digit (cols 0-2) fits, partial second
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('█'));
    }

    #[test]
    fn bigtext_fg_color() {
        let bt = BigText::new().digits(&[0]).fg(3);
        let mut buf = View::new(3, 3);
        bt.draw(Rect::new(0, 0, 3, 3), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.fg), Some(3));
    }

    #[test]
    fn bigtext_bg_fill() {
        let bt = BigText::new().digits(&[0]).fg(3).bg(4);
        let mut buf = View::new(5, 3);
        bt.draw(Rect::new(0, 0, 5, 3), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.bg), Some(4));
        assert_eq!(buf.get(0, 4).map(|c| c.bg), Some(4));
    }

    #[test]
    fn bigtext_spacing_between_digits() {
        let bt = BigText::new().digits(&[1, 2]).spacing(2);
        let mut buf = View::new(20, 3);
        bt.draw(Rect::new(0, 0, 20, 3), &mut buf);
        // Digit '1' at cols 0-2, gap at 3-4, digit '2' at cols 5-7
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('█'));
        assert_eq!(buf.get(0, 3).map(|c| c.ch), Some(' '));
        assert_eq!(buf.get(0, 5).map(|c| c.ch), Some('█'));
    }

    #[test]
    fn bigtext_zero_spacing_no_gap() {
        let bt = BigText::new().digits(&[1, 2]).spacing(0);
        let mut buf = View::new(20, 3);
        bt.draw(Rect::new(0, 0, 20, 3), &mut buf);
        // Digit '1' at cols 0-2, digit '2' at cols 3-5, no gap
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('█'));
        assert_eq!(buf.get(0, 3).map(|c| c.ch), Some('█'));
    }

    #[test]
    fn bigtext_height_too_small_noop() {
        let bt = BigText::new().digits(&[0]);
        let mut buf = View::new(3, 2);
        bt.draw(Rect::new(0, 0, 3, 2), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn block_digits_all_3x3() {
        for (d, glyph) in BLOCK_DIGITS.iter().enumerate() {
            for (row, line) in glyph.iter().enumerate() {
                assert_eq!(
                    line.chars().count(), 3,
                    "digit {} row {} must be 3 chars", d, row
                );
            }
        }
    }
}
