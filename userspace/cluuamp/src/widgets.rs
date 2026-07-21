//! TUI widgets: horizontal/vertical sliders, marquee, scrollbar, buttons.

use crate::viscolor;
use alloc::vec::Vec;
use libtui::{Cell, View, ATTR_BOLD};

pub fn draw_h_slider(
    view: &mut View,
    row: usize,
    col: usize,
    width: usize,
    value: u8,
    max: u8,
    focused: bool,
) {
    if max == 0 || width == 0 {
        return;
    }
    let filled = (value as usize * width) / max as usize;
    let fg = if focused { 226 } else { 250 };
    for i in 0..width {
        let ch = if i < filled { '\u{2588}' } else { '\u{2591}' };
        view.set(row, col + i, Cell::new(ch).fg(fg));
    }
}

pub fn draw_marquee(
    view: &mut View,
    row: usize,
    col: usize,
    width: usize,
    text: &str,
    offset: usize,
) {
    if width == 0 {
        return;
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        for i in 0..width {
            view.set(row, col + i, Cell::new(' '));
        }
        return;
    }
    let display_len = chars.len();
    if display_len <= width {
        for (i, ch) in chars.iter().enumerate() {
            view.set(row, col + i, Cell::new(*ch).fg(252));
        }
        for i in display_len..width {
            view.set(row, col + i, Cell::new(' '));
        }
        return;
    }
    let sep = "  ***  ";
    let sep_chars: Vec<char> = sep.chars().collect();
    let total_len = display_len + sep_chars.len();
    let mut full: Vec<char> = chars.clone();
    full.extend_from_slice(&sep_chars);
    for i in 0..width {
        let idx = (offset + i) % total_len;
        view.set(row, col + i, Cell::new(full[idx]).fg(252));
    }
}

pub fn draw_scrollbar(
    view: &mut View,
    row: usize,
    col: usize,
    height: usize,
    total: usize,
    visible: usize,
    offset: usize,
) {
    if height == 0 || total == 0 {
        return;
    }
    let thumb_size = ((visible * height) / total).max(1);
    let max_offset = total.saturating_sub(visible);
    let thumb_start = if max_offset == 0 {
        0
    } else {
        (offset * (height - thumb_size)) / max_offset
    };
    for i in 0..height {
        let ch = if i >= thumb_start && i < thumb_start + thumb_size {
            '\u{2588}'
        } else {
            '\u{2502}'
        };
        let fg = if i >= thumb_start && i < thumb_start + thumb_size {
            240
        } else {
            238
        };
        view.set(row + i, col, Cell::new(ch).fg(fg));
    }
}

pub fn draw_button(
    view: &mut View,
    row: usize,
    col: usize,
    label: &str,
    active: bool,
    focused: bool,
) {
    let prefix = if focused { '[' } else { ' ' };
    let suffix = if focused { ']' } else { ' ' };
    let attrs = ATTR_BOLD;
    let fg = if focused {
        226
    } else if active {
        255
    } else {
        252
    };

    view.set(
        row,
        col,
        Cell::new(prefix).fg(if focused { 226 } else { 244 }),
    );
    for (i, ch) in label.chars().enumerate() {
        view.set(row, col + 1 + i, Cell::new(ch).fg(fg).attrs(attrs));
    }
    view.set(
        row,
        col + 1 + label.chars().count(),
        Cell::new(suffix).fg(if focused { 226 } else { 244 }),
    );
}

pub fn draw_frame(
    view: &mut View,
    row: usize,
    col: usize,
    width: usize,
    height: usize,
    title: &str,
) {
    if width < 2 || height < 2 {
        return;
    }
    let top_left = '\u{2554}';
    let top_right = '\u{2557}';
    let bot_left = '\u{255A}';
    let bot_right = '\u{255D}';
    let h_line = '\u{2550}';
    let v_line = '\u{2551}';
    let fg = 238;

    view.set(row, col, Cell::new(top_left).fg(fg));
    view.set(row, col + width - 1, Cell::new(top_right).fg(fg));
    view.set(row + height - 1, col, Cell::new(bot_left).fg(fg));
    view.set(
        row + height - 1,
        col + width - 1,
        Cell::new(bot_right).fg(fg),
    );
    for i in 1..width - 1 {
        view.set(row, col + i, Cell::new(h_line).fg(fg));
        view.set(row + height - 1, col + i, Cell::new(h_line).fg(fg));
    }
    for j in 1..height - 1 {
        view.set(row + j, col, Cell::new(v_line).fg(fg));
        view.set(row + j, col + width - 1, Cell::new(v_line).fg(fg));
    }
    if !title.is_empty() {
        for (i, ch) in title.chars().enumerate() {
            if col + 2 + i < col + width - 2 {
                view.set(row, col + 2 + i, Cell::new(ch).fg(252));
            }
        }
    }
}

/// 3x3 block-digit glyphs for the Winamp-style time display (spec §2).
pub const BLOCK_DIGITS: [[&str; 3]; 10] = [
    ["█▀█", "█ █", "█▄█"], // 0
    [" █ ", " █ ", "▄█▄"], // 1
    ["█▀█", "▄▀▀", "█▄▄"], // 2
    ["█▀█", " ▀█", "█▄█"], // 3
    ["█ █", "▀▀█", "  █"], // 4
    ["█▀▀", "▀▀█", "▄▄█"], // 5
    ["█▀▀", "█▀█", "█▄█"], // 6
    ["▀▀█", "  █", "  █"], // 7
    ["█▀█", "█▀█", "█▄█"], // 8
    ["█▀█", "▀▀█", "▄▄█"], // 9
];

/// Minus sign (2 cols) and colon (1 col) glyphs for the time display.
pub const BLOCK_MINUS: [&str; 3] = ["  ", "▀▀", "  "];
pub const BLOCK_COLON: [&str; 3] = ["▄", " ", "▀"];

/// Eighth-block fill characters: index = number of filled eighths (0-8).
pub const EIGHTH_BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Fill char for `filled` eighths, clamped to 8.
pub fn eighth_block(filled: usize) -> char {
    EIGHTH_BLOCKS[filled.min(8)]
}

/// One spectrum column of the 24x3 vis box (spec §5). `level` is 0-15;
/// 3 rows = 24 vertical eighths, drawn bottom-up. Row colors
/// bottom->top come from viscolor levels 2 / 7 / 12 (blue/orange/red).
pub fn draw_spectrum_column(view: &mut View, top: usize, col: usize, level: u8) {
    let h = (level.min(15) as usize) * 24 / 15;
    for r in 0..3usize {
        let fill = (h as i32 - (2 - r as i32) * 8).clamp(0, 8) as usize;
        if fill > 0 {
            let color_level = ((2 - r) * 5 + 2) as u8;
            view.set(
                top + r,
                col,
                Cell::new(eighth_block(fill)).fg(viscolor::bar_color(color_level)),
            );
        }
    }
}

/// Three-row vertical EQ slider (spec §3). `value` in [-12,12] ->
/// filled eighths f = (value+12)*24/24 (0-24), bottom-up. Focused slider
/// renders '░' track in empty cells and a brighter fill color.
pub fn draw_eq_slider(view: &mut View, top: usize, col: usize, value: i8, focused: bool) {
    let f = (value as i32 + 12).clamp(0, 24) as usize;
    let fg = if focused { 226 } else { 46 };
    let track_fg = if focused { 226 } else { 238 };
    let fills = [f.saturating_sub(16), f.saturating_sub(8).min(8), f.min(8)];
    for (r, &fill) in fills.iter().enumerate() {
        let ch = if fill >= 8 {
            '█'
        } else if fill > 0 {
            eighth_block(fill)
        } else {
            '░'
        };
        let cell_fg = if fill > 0 { fg } else { track_fg };
        view.set(top + r, col, Cell::new(ch).fg(cell_fg));
    }
}

/// EQ curve strip glyph for a band value in [-12,12] (8 steps, spec §3).
pub fn curve_char(value: i8) -> char {
    const CURVE: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let idx = ((value as i32 + 12) * 7 / 24).clamp(0, 7) as usize;
    CURVE[idx]
}

/// Oscilloscope in a `width` x 3 box (spec §5). `points` are -32..31
/// (Oscilloscope::point()). Vertical resolution 6 half-cells:
/// y = 3 + val*6/64 clamped 0-5; upper half-cell '▀', lower '▄'.
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

/// Winamp-style block time "-mm:ss": 20 cols x 3 rows at (top, col).
/// Field layout: minus cols +0..1, digits at cols +3/+7/+13/+17 (3 wide),
/// colon at col +11. When `negative` is false the minus cells are left
/// untouched.
pub fn draw_block_time(
    view: &mut View,
    top: usize,
    col: usize,
    negative: bool,
    mins: u64,
    secs: u64,
    fg: u8,
) {
    let digits = [
        ((mins / 10) % 10) as usize,
        (mins % 10) as usize,
        ((secs / 10) % 10) as usize,
        (secs % 10) as usize,
    ];
    let digit_cols = [3usize, 7, 13, 17];
    for row in 0..3 {
        if negative {
            for (i, ch) in BLOCK_MINUS[row].chars().enumerate() {
                view.set(top + row, col + i, Cell::new(ch).fg(fg));
            }
        }
        for (i, ch) in BLOCK_COLON[row].chars().enumerate() {
            view.set(top + row, col + 11 + i, Cell::new(ch).fg(fg));
        }
        for (di, &dv) in digits.iter().enumerate() {
            for (i, ch) in BLOCK_DIGITS[dv][row].chars().enumerate() {
                view.set(top + row, col + digit_cols[di] + i, Cell::new(ch).fg(fg));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use libtui::ATTR_REVERSE;

    #[test]
    fn h_slider_zero_value_all_empty() {
        let mut v = View::new(10, 1);
        draw_h_slider(&mut v, 0, 0, 10, 0, 100, false);
        for i in 0..10 {
            let cell = v.get(0, i).unwrap();
            assert_eq!(cell.ch, '\u{2591}', "cell {} should be empty shade", i);
        }
    }

    #[test]
    fn h_slider_full_value_all_filled() {
        let mut v = View::new(10, 1);
        draw_h_slider(&mut v, 0, 0, 10, 100, 100, false);
        for i in 0..10 {
            let cell = v.get(0, i).unwrap();
            assert_eq!(cell.ch, '\u{2588}', "cell {} should be full block", i);
        }
    }

    #[test]
    fn h_slider_half_value_half_filled() {
        let mut v = View::new(10, 1);
        draw_h_slider(&mut v, 0, 0, 10, 50, 100, false);
        for i in 0..5 {
            assert_eq!(
                v.get(0, i).unwrap().ch,
                '\u{2588}',
                "first half should be filled at {}",
                i
            );
        }
        for i in 5..10 {
            assert_eq!(
                v.get(0, i).unwrap().ch,
                '\u{2591}',
                "second half should be empty at {}",
                i
            );
        }
    }

    #[test]
    fn h_slider_focused_uses_bright_color() {
        let mut v = View::new(10, 1);
        draw_h_slider(&mut v, 0, 0, 10, 50, 100, false);
        let unfocused_fg = v.get(0, 0).unwrap().fg;
        let mut v2 = View::new(10, 1);
        draw_h_slider(&mut v2, 0, 0, 10, 50, 100, true);
        let focused_fg = v2.get(0, 0).unwrap().fg;
        assert_ne!(
            unfocused_fg, focused_fg,
            "focused and unfocused should differ in color"
        );
        for col in 0..10 {
            let cell = v2.get(0, col).unwrap();
            assert_eq!(cell.fg, 226);
            assert_eq!(cell.bg, 0);
            assert_eq!(cell.attrs & ATTR_REVERSE, 0);
        }
    }

    #[test]
    fn h_slider_zero_width_noop() {
        let mut v = View::new(1, 1);
        draw_h_slider(&mut v, 0, 0, 0, 50, 100, false);
        assert_eq!(v.get(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn marquee_short_text_displayed_without_scroll() {
        let mut v = View::new(20, 1);
        draw_marquee(&mut v, 0, 0, 20, "Hello", 0);
        let s: String = (0..5).map(|i| v.get(0, i).unwrap().ch).collect();
        assert_eq!(s, "Hello");
    }

    #[test]
    fn marquee_long_text_wraps_with_separator() {
        let mut v = View::new(10, 1);
        let long = "This is a very long title that exceeds the width";
        draw_marquee(&mut v, 0, 0, 10, long, 0);
        let s: String = (0..10).map(|i| v.get(0, i).unwrap().ch).collect();
        assert!(
            s.starts_with("This is a "),
            "first 10 chars should be start of text, got '{}'",
            s
        );
        let mut v2 = View::new(10, 1);
        draw_marquee(&mut v2, 0, 0, 10, long, long.len() + 2);
        let s2: String = (0..10).map(|i| v2.get(0, i).unwrap().ch).collect();
        assert!(
            s2.starts_with("***"),
            "after text+separator boundary, should show separator, got '{}'",
            s2
        );
    }

    #[test]
    fn marquee_empty_text_fills_with_spaces() {
        let mut v = View::new(5, 1);
        draw_marquee(&mut v, 0, 0, 5, "", 0);
        for i in 0..5 {
            assert_eq!(v.get(0, i).unwrap().ch, ' ');
        }
    }

    #[test]
    fn scrollbar_thumb_position_at_top_when_offset_zero() {
        let mut v = View::new(1, 10);
        draw_scrollbar(&mut v, 0, 0, 10, 100, 5, 0);
        let top_cell = v.get(0, 0).unwrap();
        assert_eq!(top_cell.ch, '\u{2588}', "top should be thumb when offset=0");
    }

    #[test]
    fn scrollbar_thumb_position_at_bottom_when_max_offset() {
        let mut v = View::new(1, 10);
        let total = 100;
        let visible = 5;
        let max_offset = total - visible;
        draw_scrollbar(&mut v, 0, 0, 10, total, visible, max_offset);
        let bottom_cell = v.get(9, 0).unwrap();
        assert_eq!(
            bottom_cell.ch, '\u{2588}',
            "bottom should be thumb at max offset"
        );
    }

    #[test]
    fn scrollbar_empty_list_noop() {
        let mut v = View::new(1, 5);
        draw_scrollbar(&mut v, 0, 0, 5, 0, 0, 0);
    }

    #[test]
    fn button_renders_label() {
        let mut v = View::new(10, 1);
        draw_button(&mut v, 0, 0, "Play", false, false);
        assert_eq!(v.get(0, 1).unwrap().ch, 'P');
        assert_eq!(v.get(0, 2).unwrap().ch, 'l');
        assert_eq!(v.get(0, 3).unwrap().ch, 'a');
        assert_eq!(v.get(0, 4).unwrap().ch, 'y');
    }

    #[test]
    fn button_focused_has_brackets() {
        let mut v = View::new(10, 1);
        draw_button(&mut v, 0, 0, "OK", false, true);
        assert_eq!(v.get(0, 0).unwrap().ch, '[');
        assert_eq!(v.get(0, 3).unwrap().ch, ']');
        for col in 0..4 {
            let cell = v.get(0, col).unwrap();
            assert_eq!(cell.fg, 226);
            assert_eq!(cell.bg, 0);
            assert_eq!(cell.attrs & ATTR_REVERSE, 0);
        }
    }

    #[test]
    fn button_unfocused_has_spaces() {
        let mut v = View::new(10, 1);
        draw_button(&mut v, 0, 0, "OK", false, false);
        assert_eq!(v.get(0, 0).unwrap().ch, ' ');
        assert_eq!(v.get(0, 3).unwrap().ch, ' ');
    }

    #[test]
    fn frame_draws_corners() {
        let mut v = View::new(10, 5);
        draw_frame(&mut v, 0, 0, 10, 5, "Test");
        assert_eq!(v.get(0, 0).unwrap().ch, '\u{2554}', "top-left corner");
        assert_eq!(v.get(0, 9).unwrap().ch, '\u{2557}', "top-right corner");
        assert_eq!(v.get(4, 0).unwrap().ch, '\u{255A}', "bottom-left corner");
        assert_eq!(v.get(4, 9).unwrap().ch, '\u{255D}', "bottom-right corner");
    }

    #[test]
    fn frame_too_small_noop() {
        let mut v = View::new(2, 2);
        draw_frame(&mut v, 0, 0, 1, 1, "");
    }

    #[test]
    fn frame_title_overwrites_border() {
        let mut v = View::new(10, 3);
        draw_frame(&mut v, 0, 0, 10, 3, "Hi");
        assert_eq!(v.get(0, 2).unwrap().ch, 'H');
        assert_eq!(v.get(0, 3).unwrap().ch, 'i');
    }

    #[test]
    fn block_digit_glyphs_are_3x3() {
        for (d, glyph) in BLOCK_DIGITS.iter().enumerate() {
            for (r, row) in glyph.iter().enumerate() {
                assert_eq!(
                    row.chars().count(),
                    3,
                    "digit {} row {} must be 3 chars",
                    d,
                    r
                );
            }
        }
        for row in BLOCK_MINUS.iter() {
            assert_eq!(row.chars().count(), 2, "minus rows must be 2 chars");
        }
        for row in BLOCK_COLON.iter() {
            assert_eq!(row.chars().count(), 1, "colon rows must be 1 char");
        }
    }

    #[test]
    fn block_time_renders_digits_at_expected_columns() {
        // -12:34 at top=0, col=0. Field layout: minus cols 0-1, digit cols
        // 3/7/13/17 (3 wide each), colon col 11.
        let mut v = View::new(30, 4);
        draw_block_time(&mut v, 0, 0, true, 12, 34, 46);
        // minus: middle row shows "▀▀"
        assert_eq!(v.get(1, 0).unwrap().ch, '▀');
        assert_eq!(v.get(1, 1).unwrap().ch, '▀');
        // digit '1' top row is " █ " at cols 3-5
        assert_eq!(v.get(0, 4).unwrap().ch, '█');
        // digit '2' top row is "█▀█" at cols 7-9
        assert_eq!(v.get(0, 7).unwrap().ch, '█');
        assert_eq!(v.get(0, 8).unwrap().ch, '▀');
        // colon at col 11: top '▄', bottom '▀'
        assert_eq!(v.get(0, 11).unwrap().ch, '▄');
        assert_eq!(v.get(2, 11).unwrap().ch, '▀');
        // digit '3' at cols 13-15, digit '4' at cols 17-19
        assert_eq!(v.get(0, 13).unwrap().ch, '█');
        assert_eq!(v.get(2, 19).unwrap().ch, '█');
        // color
        assert_eq!(v.get(1, 0).unwrap().fg, 46);
    }

    #[test]
    fn block_time_positive_has_no_minus() {
        let mut v = View::new(30, 4);
        draw_block_time(&mut v, 0, 0, false, 0, 0, 46);
        // minus cells untouched -> default space
        assert_eq!(v.get(1, 0).unwrap().ch, ' ');
    }

    #[test]
    fn eighth_block_table() {
        assert_eq!(eighth_block(0), ' ');
        assert_eq!(eighth_block(1), '▁');
        assert_eq!(eighth_block(4), '▄');
        assert_eq!(eighth_block(8), '█');
        assert_eq!(eighth_block(99), '█'); // clamps
    }

    #[test]
    fn spectrum_column_level_15_fills_all_three_rows() {
        let mut v = View::new(4, 4);
        draw_spectrum_column(&mut v, 0, 0, 15);
        assert_eq!(v.get(0, 0).unwrap().ch, '█');
        assert_eq!(v.get(1, 0).unwrap().ch, '█');
        assert_eq!(v.get(2, 0).unwrap().ch, '█');
        assert_eq!(v.get(2, 0).unwrap().fg, crate::viscolor::bar_color(2));
        assert_eq!(v.get(1, 0).unwrap().fg, crate::viscolor::bar_color(7));
        assert_eq!(v.get(0, 0).unwrap().fg, crate::viscolor::bar_color(12));
    }

    #[test]
    fn spectrum_column_level_0_draws_nothing() {
        let mut v = View::new(4, 4);
        draw_spectrum_column(&mut v, 0, 0, 0);
        for r in 0..3 {
            assert_eq!(v.get(r, 0).unwrap().ch, ' ');
        }
    }

    #[test]
    fn spectrum_column_partial_fill_bottom_up() {
        let mut v = View::new(4, 4);
        draw_spectrum_column(&mut v, 0, 0, 8);
        assert_eq!(v.get(2, 0).unwrap().ch, '█');
        assert_eq!(v.get(1, 0).unwrap().ch, '▄');
        assert_eq!(v.get(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn eq_slider_fill_math() {
        let mut v = View::new(4, 4);
        draw_eq_slider(&mut v, 0, 0, -12, false);
        assert_eq!(v.get(0, 0).unwrap().ch, '░');
        assert_eq!(v.get(1, 0).unwrap().ch, '░');
        assert_eq!(v.get(2, 0).unwrap().ch, '░');
        let mut v = View::new(4, 4);
        draw_eq_slider(&mut v, 0, 0, 0, false);
        assert_eq!(v.get(2, 0).unwrap().ch, '█');
        assert_eq!(v.get(1, 0).unwrap().ch, '▄');
        assert_eq!(v.get(0, 0).unwrap().ch, '░');
        let mut v = View::new(4, 4);
        draw_eq_slider(&mut v, 0, 0, 12, false);
        assert_eq!(v.get(0, 0).unwrap().ch, '█');
        assert_eq!(v.get(1, 0).unwrap().ch, '█');
        assert_eq!(v.get(2, 0).unwrap().ch, '█');
    }

    #[test]
    fn eq_slider_focused_shows_track() {
        let mut v = View::new(4, 4);
        draw_eq_slider(&mut v, 0, 0, -12, true);
        assert_eq!(v.get(0, 0).unwrap().ch, '░');
        assert_eq!(v.get(1, 0).unwrap().ch, '░');
        assert_eq!(v.get(2, 0).unwrap().ch, '░');
        for row in 0..3 {
            let cell = v.get(row, 0).unwrap();
            assert_eq!(cell.fg, 226);
            assert_eq!(cell.bg, 0);
            assert_eq!(cell.attrs & ATTR_REVERSE, 0);
        }
    }

    #[test]
    fn eq_slider_unfocused_shows_track() {
        let mut v = View::new(4, 4);
        draw_eq_slider(&mut v, 0, 0, -12, false);
        assert_eq!(v.get(0, 0).unwrap().ch, '░');
        assert_eq!(v.get(1, 0).unwrap().ch, '░');
        assert_eq!(v.get(2, 0).unwrap().ch, '░');
        assert_eq!(v.get(0, 0).unwrap().fg, 238);
    }

    #[test]
    fn curve_char_range() {
        assert_eq!(curve_char(-12), '▁');
        assert_eq!(curve_char(12), '█');
    }

    #[test]
    fn scope_box_flat_line_at_center() {
        // all-zero points -> y = 3 -> row 1, lower half block
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
        // -32 -> y=0 -> row0 '▀'; 31 -> y=5 -> row2 '▄'
        assert_eq!(v.get(0, 0).unwrap().ch, '▀');
        assert_eq!(v.get(2, 1).unwrap().ch, '▄');
    }
}
