//! View rendering: flex-based Winamp three-window layout (spec §2–§5).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libtui::buffer::COLOR_DEFAULT;
use libtui::components::bigtext::BigText;
use libtui::components::button::Button;
use libtui::components::gauge::Gauge;
use libtui::components::marquee::Marquee;
use libtui::components::scrollbar::Scrollbar;
use libtui::layout::{Block as LayoutBlock, Border, Drawable, Rect};
use libtui::{Cell, View, ATTR_BOLD, ATTR_REVERSE};

use crate::audio::PlaybackState;
use crate::model::{CluuampModel, FocusArea, VisMode};
use crate::widgets;

pub fn render_into(view: &mut View, model: &mut CluuampModel) {
    let (w, h) = (model.layout.width, model.layout.height);
    view.reset(w, h);
    if !model.layout.fits() {
        draw_too_small(view, w, h);
        return;
    }
    if let Some(dialog) = &model.browser {
        dialog.draw_modal(w, h, view);
        return;
    }
    draw_main_window(view, model);
    if model.layout.show_eq {
        draw_eq_window(view, model);
    }
    if model.layout.show_playlist {
        draw_playlist_window(view, model);
    }
    draw_footer(view, model);
}

pub fn render(model: &mut CluuampModel) -> View {
    let mut view = View::new(model.layout.width, model.layout.height);
    render_into(&mut view, model);
    view
}

fn draw_too_small(view: &mut View, w: usize, h: usize) {
    let msg = "Terminal too small. Need at least 76x29.";
    let col = w.saturating_sub(msg.len()) / 2;
    let row = h / 2;
    view.write_styled(row, col, msg, Cell::new(' ').fg(196).attrs(ATTR_BOLD));
}

fn draw_main_window(view: &mut View, model: &mut CluuampModel) {
    let layout = &model.layout;
    let w = layout.width;

    // Titlebar: "CLUUAMP" centered + state label right.
    let title = " CLUUAMP ";
    let tlen = title.chars().count();
    let start = w.saturating_sub(tlen) / 2;
    let title_row = layout.main_title.y;
    view.write_styled(title_row, start, title, Cell::new(' ').fg(46).attrs(ATTR_BOLD));
    let (state_label, state_fg) = match model.audio.state() {
        PlaybackState::Playing => (" PLAYING ", 46u8),
        PlaybackState::Paused => (" PAUSED ", 226),
        PlaybackState::Stopped => (" STOPPED ", 244),
    };
    let state_col = w.saturating_sub(state_label.len() + 2);
    view.write_styled(title_row, state_col, state_label, Cell::new(' ').fg(state_fg));

    draw_three_col_border(view, model);
    draw_bigtext_content(view, model);
    draw_fft_content(view, model);
    draw_info_content(view, model);
    draw_sliders(view, model);
    draw_seekbar(view, model);
    draw_transport(view, model);
}

fn draw_three_col_border(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let area = layout.three_col_area;
    if area.width < 2 || area.height < 2 {
        return;
    }
    let fg = COLOR_DEFAULT;
    let last_row = area.bottom() - 1;
    let last_col = area.right() - 1;

    view.set(area.y, area.x, Cell::new('┌').fg(fg));
    view.set(area.y, last_col, Cell::new('┐').fg(fg));
    view.set(last_row, area.x, Cell::new('└').fg(fg));
    view.set(last_row, last_col, Cell::new('┘').fg(fg));

    for c in (area.x + 1)..last_col {
        view.set(area.y, c, Cell::new('─').fg(fg));
        view.set(last_row, c, Cell::new('─').fg(fg));
    }

    for r in (area.y + 1)..last_row {
        view.set(r, area.x, Cell::new('│').fg(fg));
        view.set(r, last_col, Cell::new('│').fg(fg));
    }

    let dividers = [layout.bigtext_box.right(), layout.fft_box.right()];
    for &dx in &dividers {
        if dx >= last_col {
            continue;
        }
        view.set(area.y, dx, Cell::new('┬').fg(fg));
        view.set(last_row, dx, Cell::new('┴').fg(fg));
        for r in (area.y + 1)..last_row {
            view.set(r, dx, Cell::new('│').fg(fg));
        }
    }
}

fn draw_bigtext_content(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let inner = layout.bigtext_inner;
    let pos = model.audio.position_ms();
    let dur = model.audio.duration_ms();
    let (negative, shown_ms) = match model.audio.state() {
        PlaybackState::Stopped => (false, pos),
        _ => (true, dur.saturating_sub(pos)),
    };
    let mins = shown_ms / 60000;
    let secs = (shown_ms / 1000) % 60;
    BigText::new()
        .time(mins, secs)
        .negative(negative)
        .fg(46)
        .spacing(1)
        .draw(inner, view);
}

fn draw_fft_content(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let inner = layout.fft_inner;
    let title = match model.vis_mode {
        VisMode::Spectrum => "Spectrum",
        VisMode::Oscilloscope => "Scope",
    };
    let title_start = inner.x + 1;
    view.write_styled(layout.three_col_area.y, title_start, title, Cell::new(' ').fg(COLOR_DEFAULT));
    match model.vis_mode {
        VisMode::Spectrum => {
            let n = model.fft.num_bars();
            let mut bar_heights = [0u8; 256];
            for i in 0..n {
                bar_heights[i] = model.fft.bar_height(i);
            }
            widgets::draw_spectrum_braille(
                view,
                inner.y,
                inner.x,
                inner.width,
                inner.height,
                &bar_heights[..n],
            );
        }
        VisMode::Oscilloscope => {
            let mut points = [0i8; 75];
            for i in 0..75 {
                points[i] = model.scope.point(i);
            }
            widgets::draw_scope_box(view, inner.y, inner.x, inner.width, &points);
        }
    }
}

fn draw_info_content(view: &mut View, model: &mut CluuampModel) {
    let layout = &model.layout;
    let inner = layout.info_inner;

    model.audio.write_display_title(model.audio.current_index(), &mut model.scratch_title);
    let mut marquee = Marquee::new(&model.scratch_title)
        .fg(252)
        .max_width(inner.width);
    marquee.set_offset(model.title_scroll_offset);
    marquee.draw(
        Rect::new(inner.x, inner.y, inner.width, 1),
        view,
    );

    let kbps = model.audio.bitrate_kbps();
    let khz = model.audio.sample_rate() / 1000;
    use core::fmt::Write;
    model.scratch_str.clear();
    if kbps == 0 {
        model.scratch_str.push_str("---");
    } else {
        let _ = write!(model.scratch_str, "{:>3}", kbps);
    }
    model.scratch_str.push_str(" kbps  ");
    if khz == 0 {
        model.scratch_str.push_str("--");
    } else {
        let _ = write!(model.scratch_str, "{:>2}", khz);
    }
    model.scratch_str.push_str(" khz");
    view.write_styled(inner.y + 1, inner.x, &model.scratch_str, Cell::new(' ').fg(244));

    let stereo = model.audio.channels() >= 2;
    let mono_fg = if stereo { 240 } else { 46 };
    let stereo_fg = if stereo { 46 } else { 240 };
    let mono_attrs = if stereo { 0 } else { ATTR_BOLD };
    let stereo_attrs = if stereo { ATTR_BOLD } else { 0 };
    view.write_styled(inner.y + 2, inner.x, "mono", Cell::new(' ').fg(mono_fg).attrs(mono_attrs));
    view.write_styled(inner.y + 2, inner.x + 6, "STEREO", Cell::new(' ').fg(stereo_fg).attrs(stereo_attrs));
}

fn draw_sliders(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let row = layout.sliders_row.y;
    let volume_focused = model.focus == FocusArea::Volume;
    let balance_focused = model.focus == FocusArea::Balance;
    view.set(
        row,
        1,
        Cell::new('V').fg(if volume_focused { 226 } else { 244 }),
    );
    let vol_fg = if volume_focused { 226 } else { 250 };
    Gauge::new(100)
        .value(model.audio.volume() as u64)
        .fg(vol_fg)
        .bg(vol_fg)
        .draw(Rect::new(3, row, 20, 1), view);
    view.set(
        row,
        25,
        Cell::new('B').fg(if balance_focused { 226 } else { 244 }),
    );
    let bal_val = (model.audio.balance() + 50) as u8;
    let bal_fg = if balance_focused { 226 } else { 250 };
    Gauge::new(100)
        .value(bal_val as u64)
        .fg(bal_fg)
        .bg(bal_fg)
        .draw(Rect::new(27, row, 14, 1), view);
    let eq_fg = if model.show_eq { 46 } else { 240 };
    view.write_styled(row, 44, "[EQ]", Cell::new(' ').fg(eq_fg).attrs(ATTR_BOLD));
    let pl_fg = if model.show_playlist { 46 } else { 240 };
    view.write_styled(row, 49, "[PL]", Cell::new(' ').fg(pl_fg).attrs(ATTR_BOLD));
}

fn draw_seekbar(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let w = layout.width;
    let row = layout.seekbar_row.y;
    let dur_nz = model.audio.duration_ms().max(1);
    let pos_pct = ((model.audio.position_ms() as u32 * 255) / dur_nz as u32).min(255) as u8;
    let seek_fg = if model.focus == FocusArea::Position { 226 } else { 250 };
    Gauge::new(255)
        .value(pos_pct as u64)
        .fg(seek_fg)
        .bg(seek_fg)
        .draw(Rect::new(1, row, w.saturating_sub(2), 1), view);
}

fn draw_transport(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let row = layout.transport_row.y;
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
        let label_len = label.chars().count();
        Button::new(label)
            .active(*active)
            .focused(focused)
            .draw(Rect::new(col, row, label_len + 2, 1), view);
        col += label.len() + 3;
    }
    col += 3;
    let shuf_focused = model.focus == FocusArea::Transport && model.transport_selected == 6;
    Button::new("SHUF")
        .active(model.shuffle)
        .focused(shuf_focused)
        .draw(Rect::new(col, row, "SHUF".chars().count() + 2, 1), view);
    col += "SHUF".len() + 3;
    let rep_focused = model.focus == FocusArea::Transport && model.transport_selected == 7;
    Button::new("REP")
        .active(model.repeat)
        .focused(rep_focused)
        .draw(Rect::new(col, row, "REP".chars().count() + 2, 1), view);
}

fn draw_eq_window(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let area = layout.eq_area;

    LayoutBlock::new()
        .border(Border::single())
        .title("EQUALIZER")
        .draw(area, view);

    let left = layout.eq_left;
    let on_fg = if model.eq_enabled { 46 } else { 240 };
    view.write_styled(left.y, left.x, "[ON]", Cell::new(' ').fg(on_fg).attrs(ATTR_BOLD));
    if left.height > 1 {
        view.write_styled(left.y + 1, left.x, "[AUTO]", Cell::new(' ').fg(240));
    }

    let right = layout.eq_right;
    let presets = "[PRESETS]";
    let py = right.y + right.height.saturating_sub(1);
    view.write_styled(py, right.x, presets, Cell::new(' ').fg(240));

    let graph = layout.eq_graph;
    let pre = model.eq_bands[0] as i32;
    let freq_bands = &model.eq_bands[1..];
    let band_count = freq_bands.len();
    let total_eighths = graph.height * 8;
    for col in 0..graph.width {
        let x = graph.x + col;
        let pos = if graph.width > 1 { col * (band_count - 1) } else { 0 };
        let lo = pos / (graph.width - 1).max(1);
        let hi = (lo + 1).min(band_count - 1);
        let remainder = pos % (graph.width - 1).max(1);
        let span = (graph.width - 1).max(1);
        let val_lo = freq_bands[lo] as i32;
        let val_hi = freq_bands[hi] as i32;
        let interpolated = val_lo + (val_hi - val_lo) * remainder as i32 / span as i32;
        let f = (interpolated + pre + 12).clamp(0, 24) as usize;
        let fill_eighths = f * total_eighths / 24;
        for row in 0..graph.height {
            let from_bottom = graph.height - 1 - row;
            let cell_start = from_bottom * 8;
            let cell_end = cell_start + 8;
            let fill = if fill_eighths >= cell_end {
                8
            } else if fill_eighths > cell_start {
                fill_eighths - cell_start
            } else {
                0
            };
            let ch = if fill >= 8 {
                '█'
            } else if fill > 0 {
                widgets::eighth_block(fill)
            } else {
                ' '
            };
            view.set(graph.y + row, x, Cell::new(ch).fg(51));
        }
    }

    let freqs = [
        "pre", "60", "170", "310", "600", "1k", "3k", "6k", "12k", "14k", "16k",
    ];
    let slider_top = layout.eq_sliders.y;
    let slider_h = layout.eq_sliders.height;
    let labels_row = layout.eq_labels.y;
    for i in 0..11 {
        let x = layout.eq_band_x[i];
        let focused = model.focus == FocusArea::Eq && model.eq_selected == i;
        let value = model.eq_bands[i];
        let fill_fg = if focused { 226 } else { 46 };
        let track_fg = if focused { 226 } else { 238 };
        Gauge::new(24)
            .value((value as i32 + 12) as u64)
            .vertical()
            .fg(fill_fg)
            .bg(track_fg)
            .track_char('░')
            .draw(Rect::new(x, slider_top, 1, slider_h), view);
        let label = freqs[i];
        let start = x.saturating_sub(label.len() / 2).max(layout.eq_inner.x);
        let label_fg = if focused { 226 } else { 244 };
        view.write_styled(labels_row, start, label, Cell::new(' ').fg(label_fg));
    }
}

fn draw_playlist_window(view: &mut View, model: &mut CluuampModel) {
    let layout = &model.layout;
    let w = layout.width;
    let pl = model.audio.playlist();
    let area = layout.playlist_area;

    use core::fmt::Write;
    model.scratch_str.clear();
    let _ = write!(model.scratch_str, "PLAYLIST ({} tracks)", pl.len());
    LayoutBlock::new()
        .border(Border::single())
        .title(&model.scratch_str)
        .draw(area, view);

    let scroll = model.playlist_scroll;
    let selected = model.playlist_selected;
    let current = model.audio.current_index();
    let content = layout.pl_content;
    let content_w = content.width;

    for row_idx in 0..content.height {
        let track_idx = scroll + row_idx;
        let row = content.y + row_idx;
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
        if focused {
            for col in content.x..content.right() {
                view.set(row, col, Cell::new(' ').fg(226));
            }
        }
        model.scratch_str.clear();
        let _ = write!(model.scratch_str, "{:>2}", (track_idx + 1).min(99));
        let num_fg = if is_current || focused { fg } else { 238 };
        view.write_styled(row, content.x + 1, &model.scratch_str, Cell::new(' ').fg(num_fg));
        if content.x + 3 < content.right() {
            view.set(
                row,
                content.x + 3,
                Cell::new('.').fg(if focused { 226 } else { 238 }),
            );
        }
        if is_current && content.x + 5 < content.right() {
            view.set(
                row,
                content.x + 5,
                Cell::new('▶').fg(if focused { 226 } else { 46 }),
            );
        }
        let max_name = content_w.saturating_sub(17);
        model.audio.write_display_title(track_idx, &mut model.scratch_title);
        view.write_styled_n(row, content.x + 7, &model.scratch_title, max_name, Cell::new(' ').fg(fg));
        let dur = if is_current {
            let live = model.audio.duration_ms();
            if live > 0 { live } else { model.audio.track_duration_ms(track_idx) }
        } else {
            model.audio.track_duration_ms(track_idx)
        };
        if dur > 0 {
            model.scratch_str.clear();
            let _ = write!(model.scratch_str, "{}:{:02}", dur / 60000, (dur / 1000) % 60);
            let dcol = content.right().saturating_sub(7);
            let dstr_fg = if focused { 226 } else if is_current { 244 } else { 240 };
            view.write_styled(row, dcol, &model.scratch_str, Cell::new(' ').fg(dstr_fg));
        }
    }

    if !pl.is_empty() && content.height > 0 && pl.len() > content.height {
        Scrollbar::new(pl.len(), content.height)
            .offset(scroll)
            .draw(layout.scrollbar, view);
    }

    let row = layout.pl_buttons.y;
    let mut col = layout.pl_buttons.x;
    for (label, fg) in [
        ("[ADD]", 252u8),
        ("[REM]", 252),
        ("[SEL]", 240),
        ("[MISC]", 240),
    ] {
        view.write_styled(row, col, label, Cell::new(' ').fg(fg));
        col += label.len() + 1;
    }
    let pos = model.audio.position_ms();
    let dur = model.audio.duration_ms();
    model.scratch_str.clear();
    let _ = write!(model.scratch_str, "{}:{:02}/{}:{:02}",
        pos / 60000, (pos / 1000) % 60,
        dur / 60000, (dur / 1000) % 60);
    let list_label = "[LIST]";
    let list_col = w.saturating_sub(list_label.len() + 2);
    let times_col = list_col.saturating_sub(model.scratch_str.len() + 1);
    view.write_styled(row, times_col, &model.scratch_str, Cell::new(' ').fg(244));
    view.write_styled(row, list_col, list_label, Cell::new(' ').fg(240));
}

fn draw_footer(view: &mut View, model: &mut CluuampModel) {
    let layout = &model.layout;
    let row = layout.footer_area.y;
    let w = layout.width;
    let focus_label = match model.focus {
        FocusArea::Transport => "Transport",
        FocusArea::Volume => "Volume",
        FocusArea::Balance => "Balance",
        FocusArea::Position => "Position",
        FocusArea::Eq => "EQ",
        FocusArea::Playlist => "Playlist",
    };
    let help = " Tab:focus[Space:play e/p:windows E:eq r:rem v:vis n/b:track o:open q:quit ";
    let help_len = help.len().min(w);
    view.write_styled(row, 0, &help[..help_len], Cell::new(' ').fg(238));
}

#[cfg(test)]
mod tests {
use super::*;
use alloc::string::String;
use libtui::components::filedialog::FileDialog;

    fn focused_model(focus: FocusArea) -> CluuampModel {
        let mut model = CluuampModel::new(
            alloc::vec![
                String::from("/host/first.mp3"),
                String::from("/host/second.mp3")
            ],
            80,
            29,
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
        let mut model = focused_model(FocusArea::Playlist);
        let view = render(&mut model);
        let pl_content = model.layout.pl_content;
        assert!(pl_content.height > 0);
        let row = pl_content.y;
        for col in pl_content.x..pl_content.right() {
            let cell = view.get(row, col).unwrap();
            assert_eq!(cell.fg, 226);
            assert_eq!(cell.attrs & ATTR_REVERSE, 0);
        }
    }

    #[test]
    fn focused_eq_slider_and_label_are_yellow_without_reverse() {
        let mut model = focused_model(FocusArea::Eq);
        let view = render(&mut model);
        let eq_sliders = model.layout.eq_sliders;
        let eq_labels = model.layout.eq_labels;
        let band_x = model.layout.eq_band_x[0];
        assert!(eq_sliders.height > 0);
        for row in eq_sliders.y..eq_sliders.bottom() {
            let cell = view.get(row, band_x).unwrap();
            assert_eq!(cell.fg, 226);
            assert_eq!(cell.attrs & ATTR_REVERSE, 0);
        }
        let label = "pre";
        let start = band_x.saturating_sub(label.len() / 2).max(model.layout.eq_inner.x);
        for col in start..start + label.len() {
            let cell = view.get(eq_labels.y, col).unwrap();
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
            let mut m = focused_model(focus);
            assert_black_without_reverse(&render(&mut m));
        }
    }

    #[test]
    fn browser_overlay_is_black_background() {
        let mut model = focused_model(FocusArea::Playlist);
        let dialog = FileDialog::open_multi("/host", 5);
        model.browser = Some(dialog);
        let view = render(&mut model);
        assert_eq!(view.get(0, 0).unwrap().bg, 0);
        assert_eq!(view.get(0, 0).unwrap().ch, ' ');
    }
}
