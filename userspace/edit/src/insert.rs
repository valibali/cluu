//! INSERT-mode keymap. See spec §7.4.

use crate::input::KeyEvent;
use crate::mode::{Editor, Mode, StepResult};

pub fn handle(state: &mut Editor, event: KeyEvent) -> StepResult {
    match event {
        KeyEvent::Esc | KeyEvent::Ctrl('[') => {
            state.undo.commit_session(state.buf.cursor, &state.buf.pieces);
            state.mode = Mode::Normal;
            StepResult::Redraw
        }
        KeyEvent::Char(c) => {
            // Encode char as UTF-8 bytes (single byte for ASCII, multi for >=128).
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            if state.undo.is_session_open() == false {
                state.undo.begin_session(state.buf.cursor, &state.buf.pieces, 0);
            }
            if let Some(_patch) = state.buf.pieces.insert(state.buf.cursor, s.as_bytes()) {
                state.buf.cursor += s.as_bytes().len();
                state.buf.mark_dirty();
            }
            StepResult::Redraw
        }
        KeyEvent::Enter => {
            if state.undo.is_session_open() == false {
                state.undo.begin_session(state.buf.cursor, &state.buf.pieces, 0);
            }
            if let Some(_patch) = state.buf.pieces.insert(state.buf.cursor, b"\n") {
                state.buf.cursor += 1;
                state.buf.mark_dirty();
            }
            StepResult::Redraw
        }
        KeyEvent::Backspace => {
            if state.buf.cursor > 0 {
                if state.undo.is_session_open() == false {
                    state.undo.begin_session(state.buf.cursor, &state.buf.pieces, 0);
                }
                state.buf.pieces.delete(state.buf.cursor - 1 .. state.buf.cursor);
                state.buf.cursor -= 1;
                state.buf.mark_dirty();
            }
            StepResult::Redraw
        }
        _ => StepResult::Continue,
    }
}
