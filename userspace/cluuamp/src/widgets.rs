//! TUI widgets: braille spectrum, oscilloscope.

use crate::viscolor;
use libtui::{Cell, View};

const BRAILLE_MASKS: [[u8; 2]; 4] = [
    [0x01, 0x08],
    [0x02, 0x10],
    [0x04, 0x20],
    [0x40, 0x80],
];

fn braille_glyph(byte: u8) -> char {
    char::from_u32(0x2800 + byte as u32).unwrap_or('\u{2800}')
}

pub fn draw_spectrum_braille(
    view: &mut View,
    top: usize,
    left: usize,
    width: usize,
    height: usize,
    bar_heights: &[u8],
) {
    if width == 0 || height == 0 || bar_heights.is_empty() {
        return;
    }
    let total_dot_cols = 2 * width;
    let total_dot_rows = 4 * height;
    let num_bars = bar_heights.len();

    for cell_col in 0..width {
        for cell_row in 0..height {
            let mut byte: u8 = 0;
            for local_col in 0..2 {
                let dot_col = 2 * cell_col + local_col;
                let bar_idx = dot_col * num_bars / total_dot_cols;
                let level = bar_heights[bar_idx.min(num_bars - 1)];
                let dots_filled = (level as usize) * total_dot_rows / 15;
                for local_row in 0..4 {
                    let dot_row_from_bottom =
                        4 * ((height - 1) - cell_row) + (3 - local_row);
                    if dot_row_from_bottom < dots_filled {
                        byte |= BRAILLE_MASKS[local_row][local_col];
                    }
                }
            }
            if byte != 0 {
                let cell_from_bottom = height - 1 - cell_row;
                let color_level = if height <= 1 {
                    7u8
                } else {
                    (cell_from_bottom * 10 / (height - 1) + 2) as u8
                };
                view.set(
                    top + cell_row,
                    left + cell_col,
                    Cell::new(braille_glyph(byte)).fg(viscolor::bar_color(color_level)),
                );
            }
        }
    }
}

pub fn draw_scope_box(view: &mut View, top: usize, left: usize, width: usize, points: &[i8]) {
    if points.is_empty() || width == 0 {
        return;
    }
    for j in 0..width {
        let idx = j * points.len() / width;
        let val = points[idx] as i32;
        let y = (3 + val * 6 / 64).clamp(0, 5) as usize;
        let row = y / 2;
        let ch = if y % 2 == 0 { '▀' } else { '▄' };
        let dist = (y as i32 - 3).unsigned_abs() as usize;
        let color = viscolor::SCOPE_COLORS[dist.min(viscolor::SCOPE_COLORS.len() - 1)];
        view.set(top + row, left + j, Cell::new(ch).fg(color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braille_spectrum_full_fill_3_high() {
        let mut v = View::new(3, 3);
        draw_spectrum_braille(&mut v, 0, 0, 1, 3, &[15]);
        for row in 0..3 {
            assert_eq!(v.get(row, 0).unwrap().ch, '\u{28FF}', "row {}", row);
        }
    }

    #[test]
    fn braille_spectrum_silence_draws_nothing() {
        let mut v = View::new(3, 3);
        draw_spectrum_braille(&mut v, 0, 0, 1, 3, &[0]);
        for row in 0..3 {
            assert_eq!(v.get(row, 0).unwrap().ch, ' ', "row {}", row);
        }
    }

    #[test]
    fn braille_spectrum_partial_fill_bottom_only() {
        let mut v = View::new(3, 3);
        draw_spectrum_braille(&mut v, 0, 0, 1, 3, &[5]);
        assert_eq!(v.get(2, 0).unwrap().ch, '\u{28FF}');
        assert_eq!(v.get(1, 0).unwrap().ch, ' ');
        assert_eq!(v.get(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn braille_spectrum_colors_match_viscolor_height3() {
        let mut v = View::new(3, 3);
        draw_spectrum_braille(&mut v, 0, 0, 1, 3, &[15]);
        assert_eq!(v.get(2, 0).unwrap().fg, viscolor::bar_color(2));
        assert_eq!(v.get(1, 0).unwrap().fg, viscolor::bar_color(7));
        assert_eq!(v.get(0, 0).unwrap().fg, viscolor::bar_color(12));
    }

    #[test]
    fn braille_spectrum_two_bars_horizontal_resolution() {
        let mut v = View::new(3, 3);
        draw_spectrum_braille(&mut v, 0, 0, 1, 3, &[15, 0]);
        assert_eq!(v.get(2, 0).unwrap().ch, '\u{2847}');
    }

    #[test]
    fn braille_spectrum_empty_inputs_noop() {
        let mut v = View::new(3, 3);
        draw_spectrum_braille(&mut v, 0, 0, 0, 3, &[15]);
        draw_spectrum_braille(&mut v, 0, 0, 1, 0, &[15]);
        draw_spectrum_braille(&mut v, 0, 0, 1, 3, &[]);
        for row in 0..3 {
            assert_eq!(v.get(row, 0).unwrap().ch, ' ', "row {}", row);
        }
    }

    #[test]
    fn braille_glyph_maps_byte_to_unicode() {
        assert_eq!(braille_glyph(0x00), '\u{2800}');
        assert_eq!(braille_glyph(0xFF), '\u{28FF}');
        assert_eq!(braille_glyph(0x01), '\u{2801}');
    }

    #[test]
    fn scope_box_flat_line_at_center() {
        let mut v = View::new(24, 4);
        let pts = [0i8; 75];
        draw_scope_box(&mut v, 0, 0, 24, &pts);
        for j in 0..24 {
            assert_eq!(v.get(1, j).unwrap().ch, '▄', "col {}", j);
            assert_eq!(v.get(0, j).unwrap().ch, ' ');
            assert_eq!(v.get(2, j).unwrap().ch, ' ');
        }
    }

    #[test]
    fn scope_box_extremes_clamp() {
        let mut v = View::new(4, 4);
        draw_scope_box(&mut v, 0, 0, 2, &[-32i8, 31]);
        assert_eq!(v.get(0, 0).unwrap().ch, '▀');
        assert_eq!(v.get(2, 1).unwrap().ch, '▄');
    }
}
