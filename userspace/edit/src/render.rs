//! Render the editor into an ANSI byte stream + send via TTY_WRITE.
//! See spec §8 and the plan's T0 findings for the SGR adaptations.
//!
//! Adaptations from raw plan (per T0 console-renderer survey):
//! - No `CSI ?25 l/h` (cursor hide/show is silently dropped) — we just
//!   position the cursor last in each frame.
//! - No `CSI 7 m` (reverse) for the status line — the console doesn't
//!   honor it. We use `CSI 47;30 m` (white bg + black fg) and reset
//!   with `CSI 0 m` at end-of-line.

extern crate alloc;
use alloc::vec::Vec;
use crate::mode::{Editor, Mode};

pub fn render(state: &mut Editor) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 * 1024);
    // Per T0 finding 4: the console renderer doesn't honor `CSI ?25 l/h`,
    // so we don't try to hide the cursor — we just position it last.
    out.extend_from_slice(b"\x1b[H");           // home
    paint_content(state, &mut out);
    paint_status(state, &mut out);
    paint_message(state, &mut out);
    place_cursor(state, &mut out);               // cursor lands here
    out
}

fn paint_content(state: &mut Editor, out: &mut Vec<u8>) {
    crate::search::refresh_matches(state);
    let total_lines = state.buf.pieces.line_count();
    let line_idx = state.buf.pieces.line_index().to_vec();
    let buf_bytes = state.buf.pieces.read_all();
    let matches = state.search.matches.clone();
    let hl_on = state.settings.hlsearch && !state.search.pattern.is_empty();

    for row in 0..state.viewport.height {
        let file_line = state.viewport.top_line + row as usize;
        let _ = write_str(out, &alloc::format!("\x1b[{};1H\x1b[K", row + 1));
        if file_line >= total_lines {
            out.push(b'~');
            continue;
        }
        let start = line_idx[file_line];
        let end = if file_line + 1 < line_idx.len() { line_idx[file_line + 1].saturating_sub(1) } else { buf_bytes.len() };
        let mut col_skipped = 0;
        let mut col_drawn = 0;
        let mut highlighted = false;
        for (i, &b) in buf_bytes[start..end].iter().enumerate() {
            if col_skipped < state.viewport.left_col { col_skipped += 1; continue; }
            if col_drawn >= state.viewport.width as usize { break; }
            let abs = start + i;
            let in_match = hl_on && matches.iter().any(|r| r.contains(&abs));
            if in_match && !highlighted {
                out.extend_from_slice(b"\x1b[33m");
                highlighted = true;
            } else if !in_match && highlighted {
                out.extend_from_slice(b"\x1b[0m");
                highlighted = false;
            }
            let display = if b >= 0x20 && b < 0x7F { b } else if b == b'\t' { b' ' } else { b'?' };
            out.push(display);
            col_drawn += 1;
        }
        if highlighted { out.extend_from_slice(b"\x1b[0m"); }
    }
}

fn paint_status(state: &mut Editor, out: &mut Vec<u8>) {
    let row = state.viewport.height + 1;
    // Per T0 finding 4: `CSI 7 m` (reverse) is a no-op in CLUU's console.
    // Use a white bg + black fg for the status line instead.
    let _ = write_str(out, &alloc::format!("\x1b[{};1H\x1b[K\x1b[47m\x1b[30m", row));
    let mode_tag = match state.mode {
        Mode::Normal      => "        ",
        Mode::Insert      => "-- INSERT --",
        Mode::VisualChar  => "-- VISUAL --",
        Mode::VisualLine  => "-- V\u{00b7}LINE --",
        Mode::OperatorPending(_) => "        ",
        Mode::ExPrompt(_) => "        ",
    };
    let path = state.buf.path.as_deref().unwrap_or("[No Name]");
    let dirty = if state.buf.dirty { "[+]" } else { "" };
    let (line, col) = state.buf.pieces.line_col(state.buf.cursor);
    let total = state.buf.pieces.line_count();
    let pct = if total <= 1 { 100 } else { (line * 100) / total };
    let left = alloc::format!("{}   {} {}", mode_tag, path, dirty);
    let right = alloc::format!("L {}:C {}  {}%", line + 1, col + 1, pct);
    let pad = (state.viewport.width as usize).saturating_sub(left.len() + right.len());
    let _ = write_str(out, &left);
    for _ in 0..pad { out.push(b' '); }
    let _ = write_str(out, &right);
    out.extend_from_slice(b"\x1b[0m");          // reset all attrs (39/49 not supported individually)
}

fn paint_message(state: &mut Editor, out: &mut Vec<u8>) {
    let row = state.viewport.height + 2;
    let _ = write_str(out, &alloc::format!("\x1b[{};1H\x1b[K", row));
    if let Some(p) = state.prompt.as_ref() {
        let prefix = match p.kind {
            crate::mode::PromptKind::Ex => ":",
            crate::mode::PromptKind::SearchFwd => "/",
            crate::mode::PromptKind::SearchBwd => "?",
        };
        out.push(prefix.as_bytes()[0]);
        let _ = write_str(out, &p.buf);
    } else {
        let _ = write_str(out, &state.message);
    }
}

fn place_cursor(state: &mut Editor, out: &mut Vec<u8>) {
    let (line, col) = state.buf.pieces.line_col(state.buf.cursor);
    let row = (line.saturating_sub(state.viewport.top_line) + 1) as u16;
    let column = (col.saturating_sub(state.viewport.left_col) + 1) as u16;
    let _ = write_str(out, &alloc::format!("\x1b[{};{}H", row, column));
}

fn write_str(out: &mut Vec<u8>, s: &str) -> Result<(), ()> {
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Send the rendered bytes to the TTY via TTY_WRITE_LABEL on the
/// stdout endpoint. The plan literal called for `libcluu::posix::write(1, ...)`,
/// but that name is the C-extern `(c_int, *const c_void, size_t) -> ssize_t`
/// shim — not Rust-friendly. Instead we use the same `send_with_payload`
/// path that the shell uses for all of its TTY output (see
/// userspace/shell/src/main.rs:121,184,281). Same wire format, same ack
/// semantics, just no FFI dance.
pub fn flush_to_tty(bytes: &[u8]) {
    let stdout = libcluu::boot::stdout();
    if stdout == 0 {
        return;
    }
    let _ = libcluu::ipc::send_with_payload(
        stdout,
        libcluu::ipc::TTY_WRITE_LABEL,
        bytes,
    );
}

/// Adjust viewport so the cursor is on screen.
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
