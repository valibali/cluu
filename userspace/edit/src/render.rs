//! Build a `libtui::View` from the editor state.
//!
//! Replaces the old CSI byte-stream renderer. The Program runtime
//! diff-renders the View against the previous frame and writes only
//! changed cells to the terminal.

extern crate alloc;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::mode::{Editor, Mode, PromptKind};
use libtui::{Cell, View, COLOR_BLACK, COLOR_WHITE, COLOR_YELLOW};

pub struct RenderData {
    pub total_lines: usize,
    pub line_idx: Vec<usize>,
    pub buf_bytes: Vec<u8>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

pub fn compute_render_data(editor: &mut Editor) -> RenderData {
    let total_lines = editor.buf.pieces.line_count();
    let line_idx = editor.buf.pieces.line_index().to_vec();
    let buf_bytes = editor.buf.pieces.read_all();
    let (cursor_line, cursor_col) = editor.buf.pieces.line_col(editor.buf.cursor);
    RenderData { total_lines, line_idx, buf_bytes, cursor_line, cursor_col }
}

pub fn build_view(state: &Editor, data: &RenderData) -> View {
    let width = state.viewport.width as usize;
    let height = (state.viewport.height as usize) + 2;
    let mut v = View::new(width, height);

    if state.settings.wrap {
        paint_content_wrapped(state, data, &mut v);
    } else {
        paint_content_scrolled(state, data, &mut v);
    }
    paint_status(state, data, &mut v);
    paint_message(state, &mut v);

    v
}

fn gutter_width(state: &Editor, total_lines: usize) -> usize {
    if state.settings.number {
        let digits = format!("{}", total_lines).len();
        digits + 1
    } else {
        0
    }
}

fn paint_content_scrolled(state: &Editor, data: &RenderData, v: &mut View) {
    let total_lines = data.total_lines;
    let line_idx = &data.line_idx;
    let buf_bytes = &data.buf_bytes;
    let matches = &state.search.matches;
    let hl_on = state.settings.hlsearch && !state.search.pattern.is_empty();

    let gutter = gutter_width(state, data.total_lines);
    let content_w = (state.viewport.width as usize).saturating_sub(gutter);

    for row in 0..state.viewport.height as usize {
        let file_line = state.viewport.top_line + row;
        if file_line >= total_lines {
            if state.settings.number {
                for g in 0..gutter {
                    v.set(row, g, Cell::new(' '));
                }
            }
            v.set(row, gutter, Cell::new('~'));
            continue;
        }
        if state.settings.number {
            let label = format!("{:>w$} ", file_line + 1, w = gutter - 1);
            v.write_str(row, 0, &label);
        }
        let start = line_idx[file_line];
        let end = if file_line + 1 < line_idx.len() {
            line_idx[file_line + 1].saturating_sub(1)
        } else {
            buf_bytes.len()
        };
        let mut col_skipped = 0;
        let mut col_drawn = 0;
        for (i, &b) in buf_bytes[start..end].iter().enumerate() {
            if col_skipped < state.viewport.left_col {
                col_skipped += 1;
                continue;
            }
            if col_drawn >= content_w {
                break;
            }
            let abs = start + i;
            let in_match = hl_on && matches.iter().any(|r| r.contains(&abs));
            let display = if b >= 0x20 && b < 0x7F {
                b as char
            } else if b == b'\t' {
                ' '
            } else {
                '?'
            };
            let mut cell = Cell::new(display);
            if in_match {
                cell = cell.fg(COLOR_YELLOW);
            }
            v.set(row, gutter + col_drawn, cell);
            col_drawn += 1;
        }
    }
}

#[allow(clippy::needless_range_loop)]
fn paint_content_wrapped(state: &Editor, data: &RenderData, v: &mut View) {
    let total_lines = data.total_lines;
    let line_idx = &data.line_idx;
    let buf_bytes = &data.buf_bytes;
    let matches = &state.search.matches;
    let hl_on = state.settings.hlsearch && !state.search.pattern.is_empty();

    let gutter = gutter_width(state, data.total_lines);
    let content_w = (state.viewport.width as usize).saturating_sub(gutter);
    let max_rows = state.viewport.height as usize;

    let mut row = 0;
    let mut file_line = state.viewport.top_line;
    while row < max_rows && file_line < total_lines {
        let start = line_idx[file_line];
        let end = if file_line + 1 < line_idx.len() {
            line_idx[file_line + 1].saturating_sub(1)
        } else {
            buf_bytes.len()
        };
        let line = &buf_bytes[start..end];
        let mut col = 0;
        let mut first_row = true;
        while col < line.len() && row < max_rows {
            if state.settings.number {
                if first_row {
                    let label = format!("{:>w$} ", file_line + 1, w = gutter - 1);
                    v.write_str(row, 0, &label);
                } else {
                    v.fill_rect(row, 0, gutter, 1, Cell::new(' '));
                }
            }
            let take = content_w.min(line.len() - col);
            for (i, &b) in line[col..col + take].iter().enumerate() {
                let abs = start + col + i;
                let in_match = hl_on && matches.iter().any(|r| r.contains(&abs));
                let display = if b >= 0x20 && b < 0x7F {
                    b as char
                } else if b == b'\t' {
                    ' '
                } else {
                    '?'
                };
                let mut cell = Cell::new(display);
                if in_match {
                    cell = cell.fg(COLOR_YELLOW);
                }
                v.set(row, gutter + i, cell);
            }
            col += take;
            first_row = false;
            row += 1;
        }
        if line.is_empty() && row < max_rows {
            if state.settings.number {
                let label = format!("{:>w$} ", file_line + 1, w = gutter - 1);
                v.write_str(row, 0, &label);
            }
            row += 1;
        }
        file_line += 1;
    }
    while row < max_rows {
        if state.settings.number {
            for g in 0..gutter {
                v.set(row, g, Cell::new(' '));
            }
        }
        v.set(row, gutter, Cell::new('~'));
        row += 1;
    }
}

fn paint_status(state: &Editor, data: &RenderData, v: &mut View) {
    let row = state.viewport.height as usize;
    let mode_tag = match state.mode {
        Mode::Normal => "        ",
        Mode::Insert => "-- INSERT --",
        Mode::VisualChar => "-- VISUAL --",
        Mode::VisualLine => "-- V.LINE --",
        Mode::OperatorPending(_) => "        ",
        Mode::ExPrompt(_) => "        ",
    };
    let path = state.buf.path.as_deref().unwrap_or("[No Name]");
    let dirty = if state.buf.dirty { "[+]" } else { "" };
    let line = data.cursor_line;
    let col = data.cursor_col;
    let total = data.total_lines;
    let pct = if total <= 1 { 100 } else { (line * 100) / total };
    let left = format!("{}   {} {}", mode_tag, path, dirty);
    let right = format!("L {}:C {}  {}%", line + 1, col + 1, pct);
    let pad = (state.viewport.width as usize).saturating_sub(left.len() + right.len());

    let bg_cell = Cell::new(' ').bg(COLOR_WHITE).fg(COLOR_BLACK);
    v.fill_rect(row, 0, state.viewport.width as usize, 1, bg_cell);
    v.write_styled(row, 0, &left, bg_cell);
    v.write_styled(row, left.chars().count() + pad, &right, bg_cell);
}

fn paint_message(state: &Editor, v: &mut View) {
    let row = (state.viewport.height as usize) + 1;
    if let Some(p) = state.prompt.as_ref() {
        let prefix = match p.kind {
            PromptKind::Ex => ":",
            PromptKind::SearchFwd => "/",
            PromptKind::SearchBwd => "?",
        };
        v.write_str(row, 0, prefix);
        v.write_str(row, 1, &p.buf);
    } else {
        v.write_str(row, 0, &state.message);
    }
}

pub fn cursor_pos(state: &Editor, data: &RenderData) -> Option<(usize, usize)> {
    let gutter = gutter_width(state, data.total_lines);
    let row = data.cursor_line.saturating_sub(state.viewport.top_line);
    let column = data.cursor_col.saturating_sub(state.viewport.left_col) + gutter;
    if row < state.viewport.height as usize {
        Some((row, column))
    } else {
        None
    }
}

pub fn ensure_cursor_visible(state: &mut Editor) {
    let (line, col) = state.buf.pieces.line_col(state.buf.cursor);
    let scrolloff = 3;
    let h = state.viewport.height as usize;
    let w = state.viewport.width as usize;

    if line < state.viewport.top_line + scrolloff {
        state.viewport.top_line = line.saturating_sub(scrolloff);
    } else if line >= state.viewport.top_line + h.saturating_sub(scrolloff) {
        state.viewport.top_line = (line + scrolloff + 1).saturating_sub(h);
    }

    if col < state.viewport.left_col {
        state.viewport.left_col = col;
    } else if col >= state.viewport.left_col + w {
        state.viewport.left_col = col + 1 - w;
    }
}
