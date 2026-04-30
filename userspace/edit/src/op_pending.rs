//! Read a motion (or doubled-operator letter for dd/yy/cc) and apply.

use crate::input::{KeyEvent, Direction};
use crate::mode::{Editor, Mode, Operator, StepResult};
use crate::{motion, ops};

pub fn handle(state: &mut Editor, event: KeyEvent, op: Operator) -> StepResult {
    if let KeyEvent::Esc = event {
        state.mode = Mode::Normal;
        return StepResult::Redraw;
    }
    let cursor = state.buf.cursor;
    // Doubled operator letter: dd, yy, cc — line operation.
    let line_op = match (op, &event) {
        (Operator::Delete, KeyEvent::Char('d')) => true,
        (Operator::Yank,   KeyEvent::Char('y')) => true,
        (Operator::Change, KeyEvent::Char('c')) => true,
        (Operator::Indent, KeyEvent::Char('>')) => true,
        (Operator::Dedent, KeyEvent::Char('<')) => true,
        _ => false,
    };
    let (start, end) = if line_op {
        let s = motion::line_start(&mut state.buf);
        let e = motion::line_end(&mut state.buf) + 1;  // include newline
        (s, e.min(state.buf.pieces.len()))
    } else {
        let target = match event {
            KeyEvent::Char('h') | KeyEvent::Arrow(Direction::Left)  => motion::left(&mut state.buf, 1),
            KeyEvent::Char('l') | KeyEvent::Arrow(Direction::Right) => motion::right(&mut state.buf, 1),
            KeyEvent::Char('w') => motion::word_forward(&mut state.buf, 1),
            KeyEvent::Char('b') => motion::word_backward(&mut state.buf, 1),
            KeyEvent::Char('0') => motion::line_start(&mut state.buf),
            KeyEvent::Char('$') => motion::line_end(&mut state.buf),
            KeyEvent::Char('G') => motion::last_line(&mut state.buf),
            KeyEvent::Char('%') => motion::match_bracket(&mut state.buf),
            _ => { state.mode = Mode::Normal; return StepResult::Redraw; }
        };
        (cursor, target)
    };
    ops::apply(state, op, start, end);
    if state.mode != Mode::Insert {
        state.mode = Mode::Normal;
    }
    StepResult::Redraw
}
