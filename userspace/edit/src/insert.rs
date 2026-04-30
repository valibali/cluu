//! INSERT-mode keymap. See spec §7.4.

extern crate alloc;

use crate::input::KeyEvent;
use crate::mode::{Editor, Mode, StepResult};

pub fn handle(state: &mut Editor, event: KeyEvent) -> StepResult {
    match event {
        KeyEvent::Esc | KeyEvent::Ctrl('[') => {
            state.undo.commit_session(state.buf.cursor, &state.buf.pieces);
            state.mode = Mode::Normal;
            StepResult::Redraw
        }
        KeyEvent::Enter => {
            ensure_session(state);
            let indent = compute_autoindent(state);
            let mut payload = alloc::vec::Vec::new();
            payload.push(b'\n');
            payload.extend_from_slice(&indent);
            state.buf.pieces.insert(state.buf.cursor, &payload);
            state.buf.cursor += payload.len();
            state.buf.mark_dirty();
            StepResult::Redraw
        }
        KeyEvent::Tab => {
            ensure_session(state);
            let bytes: alloc::vec::Vec<u8> = if state.settings.expandtab {
                core::iter::repeat(b' ').take(state.settings.tabstop as usize).collect()
            } else {
                alloc::vec![b'\t']
            };
            state.buf.pieces.insert(state.buf.cursor, &bytes);
            state.buf.cursor += bytes.len();
            state.buf.mark_dirty();
            StepResult::Redraw
        }
        KeyEvent::ShiftTab => {
            ensure_session(state);
            // Delete one indent unit at line start if present.
            let line_start = crate::motion::line_start(&mut state.buf);
            let bytes = state.buf.pieces.read_all();
            if line_start < bytes.len() {
                let unit = if state.settings.expandtab { state.settings.tabstop as usize } else { 1 };
                let mut to_drop = 0;
                for i in 0..unit {
                    if line_start + i >= bytes.len() { break; }
                    let b = bytes[line_start + i];
                    if (state.settings.expandtab && b == b' ') || (!state.settings.expandtab && b == b'\t') {
                        to_drop = i + 1;
                    } else { break; }
                }
                if to_drop > 0 {
                    state.buf.pieces.delete(line_start..line_start + to_drop);
                    if state.buf.cursor > line_start { state.buf.cursor -= to_drop.min(state.buf.cursor - line_start); }
                    state.buf.mark_dirty();
                }
            }
            StepResult::Redraw
        }
        KeyEvent::Char(c) => {
            ensure_session(state);
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            state.buf.pieces.insert(state.buf.cursor, s.as_bytes());
            state.buf.cursor += s.as_bytes().len();
            state.buf.mark_dirty();
            StepResult::Redraw
        }
        KeyEvent::Backspace => {
            if state.buf.cursor > 0 {
                ensure_session(state);
                state.buf.pieces.delete(state.buf.cursor - 1 .. state.buf.cursor);
                state.buf.cursor -= 1;
                state.buf.mark_dirty();
            }
            StepResult::Redraw
        }
        KeyEvent::Arrow(_) | KeyEvent::PageUp | KeyEvent::PageDown
        | KeyEvent::Home | KeyEvent::End | KeyEvent::Delete => StepResult::Continue,
        _ => StepResult::Continue,
    }
}

fn ensure_session(state: &mut Editor) {
    if !state.undo.is_session_open() {
        state.undo.begin_session(state.buf.cursor, &state.buf.pieces, 0);
    }
}

/// Copy the leading whitespace of the current line; if smartindent is on
/// and the previous line ends with `{` or `:`, add one more indent unit.
fn compute_autoindent(state: &mut Editor) -> alloc::vec::Vec<u8> {
    let bytes = state.buf.pieces.read_all();
    let line_start = crate::motion::line_start(&mut state.buf);
    let mut indent = alloc::vec::Vec::new();
    for i in line_start..bytes.len() {
        let b = bytes[i];
        if b == b' ' || b == b'\t' { indent.push(b); } else { break; }
    }
    if state.settings.smartindent && state.buf.cursor > 0 {
        let last = bytes[state.buf.cursor - 1];
        if last == b'{' || last == b':' {
            if state.settings.expandtab {
                for _ in 0..state.settings.tabstop { indent.push(b' '); }
            } else {
                indent.push(b'\t');
            }
        }
    }
    indent
}
