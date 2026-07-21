//! View rendering: classic Winamp three-window layout (spec §2–§5).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libtui::components::browser::BrowserRenderOptions;
use libtui::{Cell, View, ATTR_BOLD, ATTR_REVERSE};

use crate::audio::PlaybackState;
use crate::model::{CluuampModel, FocusArea, VisMode};
use crate::widgets;

pub fn render(model: &CluuampModel) -> View {
    let layout = &model.layout;
    let mut view = View::new(layout.width, layout.height);
    if !layout.fits() {
        draw_too_small(&mut view, layout.width, layout.height);
        return view;
    }
    draw_main_window(&mut view, model);
    if layout.show_eq {
        draw_eq_window(&mut view, model);
    }
    if layout.show_playlist {
        draw_playlist_window(&mut view, model);
    }
    draw_footer(&mut view, model);
    if model.browser.is_some() {
        draw_browser_overlay(&mut view, model);
    }
    view
}

fn draw_too_small(view: &mut View, w: usize, h: usize) {
    let msg = "Terminal too small. Need at least 76x25.";
    let col = w.saturating_sub(msg.len()) / 2;
    let row = h / 2;
    for (i, ch) in msg.chars().enumerate() {
        view.set(row, col + i, Cell::new(ch).fg(196).attrs(ATTR_BOLD));
    }
}

/// Winamp-style titlebar: black background, '─' fill fg 240, centered title.
fn draw_titlebar(view: &mut View, row: usize, w: usize, title: &str, title_fg: u8) {
    for i in 0..w {
        view.set(row, i, Cell::new('─').fg(240));
    }
    let tlen = title.chars().count();
    let start = w.saturating_sub(tlen) / 2;
    for (i, ch) in title.chars().enumerate() {
        view.set(row, start + i, Cell::new(ch).fg(title_fg).attrs(ATTR_BOLD));
    }
}

fn draw_main_window(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let w = layout.width;

    // Row 0: titlebar + state label right (ends 2 cols before edge)
    draw_titlebar(view, layout.main_title_row, w, " CLUUAMP ", 46);
    let (state_label, state_fg) = match model.audio.state() {
        PlaybackState::Playing => (" PLAYING ", 46u8),
        PlaybackState::Paused => (" PAUSED ", 226),
        PlaybackState::Stopped => (" STOPPED ", 244),
    };
    let state_col = w.saturating_sub(state_label.len() + 2);
    for (i, ch) in state_label.chars().enumerate() {
        view.set(
            layout.main_title_row,
            state_col + i,
            Cell::new(ch).fg(state_fg),
        );
    }

    // State glyph (row 2, col 1)
    let (glyph, glyph_fg) = match model.audio.state() {
        PlaybackState::Playing => ('▶', 46u8),
        PlaybackState::Paused => ('║', 226),
        PlaybackState::Stopped => ('■', 244),
    };
    view.set(
        layout.state_glyph_row,
        layout.state_glyph_col,
        Cell::new(glyph).fg(glyph_fg),
    );

    // Block-digit time: remaining (negative) while playing/paused,
    // elapsed (positive) when stopped.
    let pos = model.audio.position_ms();
    let dur = model.audio.duration_ms();
    let (negative, shown_ms) = match model.audio.state() {
        PlaybackState::Stopped => (false, pos),
        _ => (true, dur.saturating_sub(pos)),
    };
    let mins = shown_ms / 60000;
    let secs = (shown_ms / 1000) % 60;
    widgets::draw_block_time(
        view,
        layout.time_top,
        layout.time_col,
        negative,
        mins,
        secs,
        46,
    );

    // Vis box 24x3
    draw_visualizer(view, model);

    // Marquee (row 1, right block)
    widgets::draw_marquee(
        view,
        layout.marquee_row,
        layout.marquee_col,
        layout.marquee_width,
        model.audio.current_title(),
        model.title_scroll_offset,
    );

    // Row 2 right: kbps / khz
    let kbps = model.audio.bitrate_kbps();
    let khz = model.audio.sample_rate() / 1000;
    let kbps_str = if kbps == 0 {
        String::from("---")
    } else {
        format!("{:>3}", kbps)
    };
    let khz_str = if khz == 0 {
        String::from("--")
    } else {
        format!("{:>2}", khz)
    };
    let info = format!("{} kbps  {} khz", kbps_str, khz_str);
    for (i, ch) in info.chars().enumerate() {
        if layout.marquee_col + i < w {
            view.set(
                layout.info_row,
                layout.marquee_col + i,
                Cell::new(ch).fg(244),
            );
        }
    }

    // Row 3 right: mono / STEREO
    let stereo = model.audio.channels() >= 2;
    let mono_fg = if stereo { 240 } else { 46 };
    let stereo_fg = if stereo { 46 } else { 240 };
    let mono_attrs = if stereo { 0 } else { ATTR_BOLD };
    let stereo_attrs = if stereo { ATTR_BOLD } else { 0 };
    for (i, ch) in "mono".chars().enumerate() {
        view.set(
            layout.stereo_row,
            layout.marquee_col + i,
            Cell::new(ch).fg(mono_fg).attrs(mono_attrs),
        );
    }
    for (i, ch) in "STEREO".chars().enumerate() {
        view.set(
            layout.stereo_row,
            layout.marquee_col + 6 + i,
            Cell::new(ch).fg(stereo_fg).attrs(stereo_attrs),
        );
    }

    // Row 5: volume + balance + [EQ] [PL]
    let row = layout.sliders_row;
    let volume_focused = model.focus == FocusArea::Volume;
    let balance_focused = model.focus == FocusArea::Balance;
    view.set(
        row,
        1,
        Cell::new('V').fg(if volume_focused { 226 } else { 244 }),
    );
    widgets::draw_h_slider(view, row, 3, 20, model.audio.volume(), 100, volume_focused);
    view.set(
        row,
        25,
        Cell::new('B').fg(if balance_focused { 226 } else { 244 }),
    );
    let bal_val = (model.audio.balance() + 50) as u8;
    widgets::draw_h_slider(view, row, 27, 14, bal_val, 100, balance_focused);
    let eq_fg = if model.show_eq { 46 } else { 240 };
    for (i, ch) in "[EQ]".chars().enumerate() {
        view.set(row, 44 + i, Cell::new(ch).fg(eq_fg).attrs(ATTR_BOLD));
    }
    let pl_fg = if model.show_playlist { 46 } else { 240 };
    for (i, ch) in "[PL]".chars().enumerate() {
        view.set(row, 49 + i, Cell::new(ch).fg(pl_fg).attrs(ATTR_BOLD));
    }

    // Row 7: seekbar
    let dur_nz = model.audio.duration_ms().max(1);
    let pos_pct = ((model.audio.position_ms() as u32 * 255) / dur_nz as u32).min(255) as u8;
    widgets::draw_h_slider(
        view,
        layout.position_row,
        1,
        w.saturating_sub(2),
        pos_pct,
        255,
        model.focus == FocusArea::Position,
    );

    // Row 9: transport (0=prev 1=play 2=pause 3=stop 4=next 5=eject 6=shuf 7=rep)
    let row = layout.transport_row;
    let state = model.audio.state();
    let buttons: [(&str, bool); 6] = [
        ("|<", false),
        (">", state == PlaybackState::Playing),
        ("||", state == PlaybackState::Paused),
        ("[]", state == PlaybackState::Stopped),
        (">|", false),
        ("^", false),
    ];
    let mut col = 1;
    for (i, (label, active)) in buttons.iter().enumerate() {
        let focused = model.focus == FocusArea::Transport && model.transport_selected == i;
        widgets::draw_button(view, row, col, label, *active, focused);
        col += label.len() + 3;
    }
    col += 3;
    let shuf_focused = model.focus == FocusArea::Transport && model.transport_selected == 6;
    widgets::draw_button(view, row, col, "SHUF", model.shuffle, shuf_focused);
    col += "SHUF".len() + 3;
    let rep_focused = model.focus == FocusArea::Transport && model.transport_selected == 7;
    widgets::draw_button(view, row, col, "REP", model.repeat, rep_focused);
}

fn draw_visualizer(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    match model.vis_mode {
        VisMode::Spectrum => {
            for j in 0..layout.vis_width {
                let bar_idx = j * 75 / layout.vis_width;
                widgets::draw_spectrum_column(
                    view,
                    layout.vis_top,
                    layout.vis_left + j,
                    model.fft.bar_height(bar_idx),
                    model.fft.peak_height(bar_idx),
                );
            }
        }
        VisMode::Oscilloscope => {
            let mut points = [0i8; 75];
            for i in 0..75 {
                points[i] = model.scope.point(i);
            }
            widgets::draw_scope_box(
                view,
                layout.vis_top,
                layout.vis_left,
                layout.vis_width,
                &points,
            );
        }
    }
}

fn draw_eq_window(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let w = layout.width;
    draw_titlebar(view, layout.eq_title_row, w, " EQUALIZER ", 252);

    // Buttons row: [ON] [AUTO] ... curve (cols 20-41) ... [PRESETS]
    let row = layout.eq_buttons_row;
    let on_fg = if model.eq_enabled { 46 } else { 240 };
    for (i, ch) in "[ON]".chars().enumerate() {
        view.set(row, 2 + i, Cell::new(ch).fg(on_fg).attrs(ATTR_BOLD));
    }
    for (i, ch) in "[AUTO]".chars().enumerate() {
        view.set(row, 7 + i, Cell::new(ch).fg(240));
    }
    for (b, &val) in model.eq_bands.iter().enumerate() {
        let ch = widgets::curve_char(val);
        view.set(row, 20 + b * 2, Cell::new(ch).fg(51));
        view.set(row, 20 + b * 2 + 1, Cell::new(ch).fg(51));
    }
    let presets = "[PRESETS]";
    let pcol = w.saturating_sub(presets.len() + 3);
    for (i, ch) in presets.chars().enumerate() {
        view.set(row, pcol + i, Cell::new(ch).fg(240));
    }

    // Sliders (3 rows) + labels
    let freqs = [
        "pre", "60", "170", "310", "600", "1k", "3k", "6k", "12k", "14k", "16k",
    ];
    for i in 0..11 {
        let x = layout.eq_band_x[i];
        let focused = model.focus == FocusArea::Eq && model.eq_selected == i;
        widgets::draw_eq_slider(view, layout.eq_slider_top, x, model.eq_bands[i], focused);
        let label = freqs[i];
        let start = x.saturating_sub(label.len() / 2);
        for (j, ch) in label.chars().enumerate() {
            if start + j < w {
                view.set(
                    layout.eq_labels_row,
                    start + j,
                    Cell::new(ch).fg(if focused { 226 } else { 244 }),
                );
            }
        }
    }
}

fn draw_playlist_window(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let w = layout.width;
    let pl = model.audio.playlist();
    draw_titlebar(
        view,
        layout.pl_title_row,
        w,
        &format!(" PLAYLIST ({} tracks) ", pl.len()),
        252,
    );

    let scroll = model.playlist_scroll;
    let selected = model.playlist_selected;
    let current = model.audio.current_index();

    for row_idx in 0..layout.playlist_height {
        let track_idx = scroll + row_idx;
        let row = layout.playlist_top + row_idx;
        if track_idx >= pl.len() {
            continue;
        }
        let is_current = track_idx == current;
        let is_selected = track_idx == selected;
        let focused = model.focus == FocusArea::Playlist && is_selected;
        let fg = if focused {
            226
        } else if is_current {
            46
        } else {
            252
        };
        let attrs = 0;
        if focused {
            for col in 0..w {
                view.set(row, col, Cell::new(' ').fg(226));
            }
        }
        // track number, right-aligned in cols 1-2, '.' at col 3
        let num = format!("{:>2}", (track_idx + 1).min(99));
        for (i, ch) in num.chars().enumerate() {
            view.set(
                row,
                1 + i,
                Cell::new(ch)
                    .fg(if is_current || focused { fg } else { 238 })
                    .attrs(attrs),
            );
        }
        view.set(
            row,
            3,
            Cell::new('.')
                .fg(if focused { 226 } else { 238 })
                .attrs(attrs),
        );
        // current marker at col 5
        if is_current {
            view.set(
                row,
                5,
                Cell::new('▶')
                    .fg(if focused { 226 } else { 46 })
                    .attrs(attrs),
            );
        }
        // name cols 7 .. w-10
        let max_name = w.saturating_sub(17); // 7 + name + gap + duration(5) + margin
        let name: String = pl[track_idx]
            .rsplit('/')
            .next()
            .unwrap_or(&pl[track_idx])
            .chars()
            .take(max_name)
            .collect();
        for (i, ch) in name.chars().enumerate() {
            view.set(row, 7 + i, Cell::new(ch).fg(fg).attrs(attrs));
        }
        // duration (current track only) cols w-7 .. w-3
        if is_current {
            let dur = model.audio.duration_ms();
            let dstr = format!("{}:{:02}", dur / 60000, (dur / 1000) % 60);
            let dcol = w.saturating_sub(7);
            for (i, ch) in dstr.chars().enumerate() {
                if dcol + i <= w.saturating_sub(3) {
                    view.set(
                        row,
                        dcol + i,
                        Cell::new(ch)
                            .fg(if focused { 226 } else { 244 })
                            .attrs(attrs),
                    );
                }
            }
        }
    }

    if !pl.is_empty() && layout.playlist_height > 0 && pl.len() > layout.playlist_height {
        widgets::draw_scrollbar(
            view,
            layout.playlist_top,
            layout.scrollbar_col,
            layout.playlist_height,
            pl.len(),
            layout.playlist_height,
            scroll,
        );
    }

    // Bottom bar
    let row = layout.pl_buttons_row;
    let mut col = 1;
    for (label, fg) in [
        ("[ADD]", 252u8),
        ("[REM]", 252),
        ("[SEL]", 240),
        ("[MISC]", 240),
    ] {
        for (i, ch) in label.chars().enumerate() {
            view.set(row, col + i, Cell::new(ch).fg(fg));
        }
        col += label.len() + 1;
    }
    let pos = model.audio.position_ms();
    let dur = model.audio.duration_ms();
    let times = format!(
        "{}:{:02}/{}:{:02}",
        pos / 60000,
        (pos / 1000) % 60,
        dur / 60000,
        (dur / 1000) % 60
    );
    let list_label = "[LIST]";
    let list_col = w.saturating_sub(list_label.len() + 2);
    let times_col = list_col.saturating_sub(times.len() + 1);
    for (i, ch) in times.chars().enumerate() {
        view.set(row, times_col + i, Cell::new(ch).fg(244));
    }
    for (i, ch) in list_label.chars().enumerate() {
        view.set(row, list_col + i, Cell::new(ch).fg(240));
    }
}

fn draw_footer(view: &mut View, model: &CluuampModel) {
    let row = model.layout.footer_row;
    let w = model.layout.width;
    let focus_label = match model.focus {
        FocusArea::Transport => "Transport",
        FocusArea::Volume => "Volume",
        FocusArea::Balance => "Balance",
        FocusArea::Position => "Position",
        FocusArea::Eq => "EQ",
        FocusArea::Playlist => "Playlist",
    };
    let help = format!(
        " Tab:focus[{focus_label}] Space:play e/p:windows E:eq r:rem v:vis n/b:track o:open q:quit "
    );
    let help_chars: Vec<char> = help.chars().collect();
    for i in 0..w.min(help_chars.len()) {
        view.set(row, i, Cell::new(help_chars[i]).fg(238));
    }
}

fn draw_browser_overlay(view: &mut View, model: &CluuampModel) {
    let w = model.layout.width;
    let h = model.layout.height;

    for r in 0..h {
        for c in 0..w {
            if r < view.height && c < view.width {
                let cell = view.get(r, c).unwrap();
                view.set(
                    r,
                    c,
                    Cell::new(cell.ch)
                        .fg(cell.fg)
                        .attrs(cell.attrs & !ATTR_REVERSE),
                );
            }
        }
    }

    let box_w = (w * 4 / 5).min(80).max(40);
    let box_h = (h * 4 / 5).min(24).max(10);
    let col = w.saturating_sub(box_w) / 2;
    let row = h.saturating_sub(box_h) / 2;

    for r in row..row + box_h {
        for c in col..col + box_w {
            if r < view.height && c < view.width {
                view.set(r, c, Cell::new(' ').fg(252).bg(8));
            }
        }
    }

    if let Some(browser) = &model.browser {
        browser.render_with_options(
            row,
            col,
            box_w,
            box_h,
            view,
            BrowserRenderOptions::borderless(8),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use libtui::components::browser::FileBrowser;

    fn focused_model(focus: FocusArea) -> CluuampModel {
        let mut model = CluuampModel::new(
            alloc::vec![
                String::from("/host/first.mp3"),
                String::from("/host/second.mp3")
            ],
            80,
            25,
        );
        model.focus = focus;
        model
    }

    fn assert_black_without_reverse(view: &View) {
        for cell in &view.cells {
            assert_eq!(cell.bg, 0);
            assert_eq!(cell.attrs & ATTR_REVERSE, 0);
        }
    }

    #[test]
    fn focused_playlist_row_is_yellow_without_reverse() {
        let view = render(&focused_model(FocusArea::Playlist));
        for col in 0..80 {
            let cell = view.get(17, col).unwrap();
            assert_eq!(cell.fg, 226);
            assert_eq!(cell.attrs & ATTR_REVERSE, 0);
        }
    }

    #[test]
    fn focused_eq_slider_and_label_are_yellow_without_reverse() {
        let view = render(&focused_model(FocusArea::Eq));
        for row in 13..16 {
            let cell = view.get(row, 3).unwrap();
            assert_eq!(cell.fg, 226);
            assert_eq!(cell.attrs & ATTR_REVERSE, 0);
        }
        for col in 2..5 {
            let cell = view.get(16, col).unwrap();
            assert_eq!(cell.fg, 226);
            assert_eq!(cell.attrs & ATTR_REVERSE, 0);
        }
    }

    #[test]
    fn render_uses_black_background_without_reverse_for_every_focus_area() {
        for focus in [
            FocusArea::Transport,
            FocusArea::Volume,
            FocusArea::Balance,
            FocusArea::Position,
            FocusArea::Eq,
            FocusArea::Playlist,
        ] {
            assert_black_without_reverse(&render(&focused_model(focus)));
        }
    }

    #[test]
    fn browser_overlay_is_borderless_gray_and_preserves_cursor() {
        let mut model = focused_model(FocusArea::Playlist);
        let mut browser = FileBrowser::new("/host", 5, true);
        browser.set_entries(alloc::vec![
            libtui::components::browser::DirEntry::file("first.mp3", 1),
            libtui::components::browser::DirEntry::file("second.mp3", 1),
        ]);
        browser.handle_key(libtui::input::KeyEvent::Arrow(
            libtui::input::Direction::Down,
        ));
        model.browser = Some(browser);
        let view = render(&model);
        assert_eq!(view.get(2, 8).unwrap().bg, 8);
        assert_ne!(view.get(2, 8).unwrap().ch, '╔');
        assert_eq!(view.get(5, 8).unwrap().ch, '>');
        assert_eq!(view.get(5, 8).unwrap().fg, 33);
    }
}
