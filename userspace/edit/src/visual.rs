//! VISUAL-char and VISUAL-line keymap. Selection is (anchor, cursor);
//! operators apply on the byte range between them.

use crate::input::{KeyEvent, Direction};
use crate::mode::{Editor, Mode, Operator, StepResult};
use crate::{motion, ops};

pub fn handle(state: &mut Editor, event: KeyEvent) -> StepResult {
    let visual_mode = state.mode;  // VisualChar or VisualLine
    match event {
        KeyEvent::Esc | KeyEvent::Char('v') | KeyEvent::Char('V') => {
            // v in VisualChar exits; v in VisualLine switches; same for V.
            state.last_visual_range = Some((state.visual_anchor.min(state.buf.cursor),
                                            state.visual_anchor.max(state.buf.cursor),
                                            visual_mode));
            state.mode = match (visual_mode, event) {
                (Mode::VisualChar, KeyEvent::Char('V')) => Mode::VisualLine,
                (Mode::VisualLine, KeyEvent::Char('v')) => Mode::VisualChar,
                _ => Mode::Normal,
            };
            StepResult::Redraw
        }
        KeyEvent::Char('o') => {
            let tmp = state.buf.cursor;
            state.buf.cursor = state.visual_anchor;
            state.visual_anchor = tmp;
            StepResult::Redraw
        }
        // Movement keys extend selection.
        KeyEvent::Char('h') | KeyEvent::Arrow(Direction::Left)  => { state.buf.cursor = motion::left(&mut state.buf, 1); StepResult::Redraw }
        KeyEvent::Char('l') | KeyEvent::Arrow(Direction::Right) => { state.buf.cursor = motion::right(&mut state.buf, 1); StepResult::Redraw }
        KeyEvent::Char('j') | KeyEvent::Arrow(Direction::Down)  => { state.buf.cursor = motion::down(&mut state.buf, 1); StepResult::Redraw }
        KeyEvent::Char('k') | KeyEvent::Arrow(Direction::Up)    => { state.buf.cursor = motion::up(&mut state.buf, 1); StepResult::Redraw }
        KeyEvent::Char('w') => { state.buf.cursor = motion::word_forward(&mut state.buf, 1); StepResult::Redraw }
        KeyEvent::Char('b') => { state.buf.cursor = motion::word_backward(&mut state.buf, 1); StepResult::Redraw }
        KeyEvent::Char('0') => { state.buf.cursor = motion::line_start(&mut state.buf); StepResult::Redraw }
        KeyEvent::Char('$') => { state.buf.cursor = motion::line_end(&mut state.buf); StepResult::Redraw }
        KeyEvent::Char('G') => { state.buf.cursor = motion::last_line(&mut state.buf); StepResult::Redraw }
        // Operators apply on selection then exit.
        KeyEvent::Char('d') => { apply_op(state, Operator::Delete, visual_mode); StepResult::Redraw }
        KeyEvent::Char('y') => { apply_op(state, Operator::Yank, visual_mode);   StepResult::Redraw }
        KeyEvent::Char('c') => { apply_op(state, Operator::Change, visual_mode); StepResult::Redraw }
        KeyEvent::Char('>') => { apply_op(state, Operator::Indent, visual_mode); StepResult::Redraw }
        KeyEvent::Char('<') => { apply_op(state, Operator::Dedent, visual_mode); StepResult::Redraw }
        _ => StepResult::Continue,
    }
}

fn apply_op(state: &mut Editor, op: Operator, visual_mode: Mode) {
    let (mut lo, mut hi) = (state.visual_anchor.min(state.buf.cursor),
                            state.visual_anchor.max(state.buf.cursor));
    if visual_mode == Mode::VisualLine {
        let (line_lo, _) = state.buf.pieces.line_col(lo);
        let (line_hi, _) = state.buf.pieces.line_col(hi);
        let idx = state.buf.pieces.line_index().to_vec();
        lo = idx[line_lo];
        hi = if line_hi + 1 < idx.len() { idx[line_hi + 1] } else { state.buf.pieces.len() };
    } else {
        hi = (hi + 1).min(state.buf.pieces.len()); // include cursor char
    }
    state.last_visual_range = Some((lo, hi, visual_mode));
    ops::apply(state, op, lo, hi);
    if state.mode != Mode::Insert {
        state.mode = Mode::Normal;
    }
}
