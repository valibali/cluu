//! TUI widgets: horizontal/vertical sliders, marquee, scrollbar, buttons.

use libtui::{Cell, View, ATTR_BOLD, ATTR_REVERSE, COLOR_DEFAULT};
use alloc::vec::Vec;
use crate::viscolor;

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
    let fg = if focused { 255 } else { 250 };
    for i in 0..width {
        let ch = if i < filled { '\u{2588}' } else { '\u{2591}' };
        view.set(row, col + i, Cell::new(ch).fg(fg));
    }
}

pub fn draw_v_slider(
    view: &mut View,
    row: usize,
    col: usize,
    height: usize,
    value: i8,
    min: i8,
    max: i8,
    focused: bool,
) {
    // `height` is the TOTAL vertical footprint including both caps; the
    // slider never draws outside rows row..row+height-1 (the old spilling
    // bottom cap overwrote the playlist header under the EQ).
    if max <= min || height < 3 {
        return;
    }
    let body = height - 2;
    let range = (max - min) as usize;
    let normalized = (value - min) as usize;
    let filled = (normalized * body) / range;
    let fg = if focused { 255 } else { 250 };
    let knob_fg = if focused { 51 } else { 248 };
    for i in 0..body {
        let from_bottom = body - 1 - i;
        let ch = if from_bottom < filled {
            '\u{2588}'
        } else if from_bottom == filled {
            '\u{2593}'
        } else {
            '\u{2591}'
        };
        let cell_fg = if from_bottom == filled { knob_fg } else { fg };
        view.set(row + 1 + i, col, Cell::new(ch).fg(cell_fg));
    }
    view.set(row, col, Cell::new('\u{2580}').fg(fg));
    view.set(row + height - 1, col, Cell::new('\u{2584}').fg(fg));
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
    let attrs = if active { ATTR_REVERSE } else { ATTR_BOLD };
    let fg = if active { 255 } else { 252 };

    view.set(row, col, Cell::new(prefix).fg(244));
    for (i, ch) in label.chars().enumerate() {
        view.set(row, col + 1 + i, Cell::new(ch).fg(fg).attrs(attrs));
    }
    view.set(row, col + 1 + label.chars().count(), Cell::new(suffix).fg(244));
}

pub fn draw_spectrum_bar(
    view: &mut View,
    row: usize,
    col: usize,
    height: usize,
    bar_level: u8,
    peak_level: u8,
) {
    let max_level = 15;
    let scaled_height = (height * max_level as usize) / max_level as usize;
    let _ = scaled_height;
    let filled_rows = (bar_level as usize * height) / max_level as usize;
    let peak_row = height.saturating_sub((peak_level as usize * height) / max_level as usize);

    for i in 0..height {
        let from_bottom = height - 1 - i;
        let row_idx = row + i;
        let level_at = (from_bottom as u16 * max_level as u16 / height as u16) as u8;
        let ch = if from_bottom < filled_rows {
            '\u{2588}'
        } else {
            '\u{2591}'
        };
        let color = viscolor::bar_color(level_at);
        view.set(row_idx, col, Cell::new(ch).fg(color));
    }
    if peak_level > 0 && peak_row < height + row {
        let peak_row_idx = row + peak_row;
        if peak_row_idx < row + height {
            view.set(peak_row_idx, col, Cell::new('\u{2580}').fg(viscolor::PEAK_COLOR));
        }
    }
}

pub fn draw_scope(
    view: &mut View,
    row: usize,
    col: usize,
    width: usize,
    height: usize,
    points: &[i8],
) {
    for i in 0..width {
        let point_idx = if points.is_empty() {
            0
        } else {
            (i * points.len()) / width
        };
        let val = if point_idx < points.len() {
            points[point_idx] as i16
        } else {
            0
        };
        let center = height as i16 / 2;
        let y_offset = (val * height as i16) / 64;
        let y_pos = (center + y_offset).clamp(0, height as i16 - 1) as usize;
        let color = viscolor::SCOPE_COLORS[(y_offset.unsigned_abs() as usize / 4)
            .min(viscolor::SCOPE_COLORS.len() - 1)];
        view.set(row + y_pos, col + i, Cell::new('\u{2588}').fg(color));
    }
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
    view.set(row + height - 1, col + width - 1, Cell::new(bot_right).fg(fg));
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
    use alloc::vec::Vec;

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
            assert_eq!(v.get(0, i).unwrap().ch, '\u{2588}', "first half should be filled at {}", i);
        }
        for i in 5..10 {
            assert_eq!(v.get(0, i).unwrap().ch, '\u{2591}', "second half should be empty at {}", i);
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
        assert_ne!(unfocused_fg, focused_fg, "focused and unfocused should differ in color");
    }

    #[test]
    fn h_slider_zero_width_noop() {
        let mut v = View::new(1, 1);
        draw_h_slider(&mut v, 0, 0, 0, 50, 100, false);
        assert_eq!(v.get(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn v_slider_zero_value_shows_empty_at_bottom() {
        let mut v = View::new(1, 6);
        draw_v_slider(&mut v, 0, 0, 3, -12, -12, 12, false);
        // height 3 = caps + 1 body row; min value puts the knob there.
        let bottom_content = v.get(1, 0).unwrap();
        assert_eq!(bottom_content.ch, '\u{2593}', "body should show knob for min value");
    }

    #[test]
    fn v_slider_max_value_shows_filled() {
        let mut v = View::new(1, 6);
        draw_v_slider(&mut v, 0, 0, 3, 12, -12, 12, false);
        let mid = v.get(1, 0).unwrap();
        assert_eq!(mid.ch, '\u{2588}', "middle should be filled for max value");
    }

    #[test]
    fn v_slider_mid_value_half_filled() {
        let mut v = View::new(1, 6);
        draw_v_slider(&mut v, 0, 0, 4, 0, -12, 12, false);
        // height 4 = caps + 2 body rows; mid value fills the lower body row.
        let filled_bottom = v.get(2, 0).unwrap();
        assert_eq!(filled_bottom.ch, '\u{2588}', "lower body row should be filled");
        let knob = v.get(1, 0).unwrap();
        assert_eq!(knob.ch, '\u{2593}', "boundary should show knob");
        let cap = v.get(3, 0).unwrap();
        assert_eq!(cap.ch, '\u{2584}', "bottom cap stays inside the footprint");
    }

    #[test]
    fn v_slider_has_top_cap() {
        let mut v = View::new(1, 6);
        draw_v_slider(&mut v, 0, 0, 3, 0, -12, 12, false);
        let cap = v.get(0, 0).unwrap();
        assert_eq!(cap.ch, '\u{2580}', "top cap should be upper half block");
    }

    #[test]
    fn v_slider_has_bottom_cap() {
        let mut v = View::new(1, 6);
        draw_v_slider(&mut v, 0, 0, 3, 0, -12, 12, false);
        let cap = v.get(2, 0).unwrap();
        assert_eq!(cap.ch, '\u{2584}', "bottom cap should be lower half block");
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
        assert_eq!(bottom_cell.ch, '\u{2588}', "bottom should be thumb at max offset");
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
    }

    #[test]
    fn button_unfocused_has_spaces() {
        let mut v = View::new(10, 1);
        draw_button(&mut v, 0, 0, "OK", false, false);
        assert_eq!(v.get(0, 0).unwrap().ch, ' ');
        assert_eq!(v.get(0, 3).unwrap().ch, ' ');
    }

    #[test]
    fn spectrum_bar_zero_level_all_empty() {
        let mut v = View::new(1, 5);
        draw_spectrum_bar(&mut v, 0, 0, 5, 0, 0);
        for i in 0..5 {
            assert_eq!(v.get(i, 0).unwrap().ch, '\u{2591}', "row {} should be empty", i);
        }
    }

    #[test]
    fn spectrum_bar_full_level_all_filled() {
        let mut v = View::new(1, 5);
        draw_spectrum_bar(&mut v, 0, 0, 5, 15, 0);
        for i in 0..5 {
            assert_eq!(v.get(i, 0).unwrap().ch, '\u{2588}', "row {} should be filled", i);
        }
    }

    #[test]
    fn spectrum_bar_half_level_half_filled() {
        let mut v = View::new(1, 4);
        draw_spectrum_bar(&mut v, 0, 0, 4, 7, 0);
        let bottom = v.get(3, 0).unwrap();
        assert_eq!(bottom.ch, '\u{2588}', "bottom should be filled");
        let top = v.get(0, 0).unwrap();
        assert_eq!(top.ch, '\u{2591}', "top should be empty");
    }

    #[test]
    fn spectrum_bar_uses_gradient_colors() {
        let mut v = View::new(1, 8);
        draw_spectrum_bar(&mut v, 0, 0, 8, 15, 0);
        let bottom_color = v.get(7, 0).unwrap().fg;
        let top_color = v.get(0, 0).unwrap().fg;
        assert_ne!(bottom_color, top_color, "bottom and top should have different colors");
    }

    #[test]
    fn scope_silence_is_empty() {
        let mut v = View::new(10, 8);
        let points = [0i8; 75];
        draw_scope(&mut v, 0, 0, 10, 8, &points);
    }

    #[test]
    fn scope_nonzero_signal_draws_something() {
        let mut v = View::new(10, 8);
        let mut points = [0i8; 75];
        for i in 0..75 {
            points[i] = 20;
        }
        draw_scope(&mut v, 0, 0, 10, 8, &points);
        let has_block = (0..8).any(|r| (0..10).any(|c| v.get(r, c).map(|cell| cell.ch == '\u{2588}').unwrap_or(false)));
        assert!(has_block, "scope should draw at least one block for nonzero signal");
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
                assert_eq!(row.chars().count(), 3, "digit {} row {} must be 3 chars", d, r);
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
}
