//! NORMAL-mode keymap with count accumulator and full motion set.
//! See spec §7.2 + §7.3.

use crate::input::{KeyEvent, Direction};
use crate::mode::{Editor, Mode, Operator, PromptKind, StepResult};
use crate::motion;
use crate::ops;

pub struct NormalAccum {
    pub count: Option<usize>,
    pub pending_g: bool,
}

impl NormalAccum {
    pub fn new() -> Self { NormalAccum { count: None, pending_g: false } }
    pub fn take_count(&mut self) -> usize { self.count.take().unwrap_or(1) }
}

pub fn handle(state: &mut Editor, event: KeyEvent) -> StepResult {
    let count_in_progress = state.normal_accum.count.is_some();

    // Digit prefix.
    if let KeyEvent::Char(c) = event {
        if c.is_ascii_digit() && (c != '0' || count_in_progress) {
            let n = state.normal_accum.count.unwrap_or(0) * 10 + (c as u8 - b'0') as usize;
            state.normal_accum.count = Some(n);
            return StepResult::Continue;
        }
    }

    // Pending g.
    if state.normal_accum.pending_g {
        state.normal_accum.pending_g = false;
        if let KeyEvent::Char('g') = event {
            state.buf.cursor = motion::first_line(&mut state.buf);
            return StepResult::Redraw;
        }
        if let KeyEvent::Char('v') = event {
            if let Some((lo, hi, m)) = state.last_visual_range {
                state.visual_anchor = lo;
                state.buf.cursor = hi.saturating_sub(1);
                state.mode = m;
            }
            return StepResult::Redraw;
        }
        // (gd handled in Task 27.)
        return StepResult::Redraw;
    }

    let count = state.normal_accum.take_count();

    match event {
        KeyEvent::Char('h') | KeyEvent::Arrow(Direction::Left)  => state.buf.cursor = motion::left(&mut state.buf, count),
        KeyEvent::Char('l') | KeyEvent::Arrow(Direction::Right) => state.buf.cursor = motion::right(&mut state.buf, count),
        KeyEvent::Char('j') | KeyEvent::Arrow(Direction::Down)  => state.buf.cursor = motion::down(&mut state.buf, count),
        KeyEvent::Char('k') | KeyEvent::Arrow(Direction::Up)    => state.buf.cursor = motion::up(&mut state.buf, count),
        KeyEvent::Char('0')                                       => state.buf.cursor = motion::line_start(&mut state.buf),
        KeyEvent::Char('$') | KeyEvent::End                       => state.buf.cursor = motion::line_end(&mut state.buf),
        KeyEvent::Home                                            => state.buf.cursor = motion::line_start(&mut state.buf),
        KeyEvent::Char('G')                                       => state.buf.cursor = motion::last_line(&mut state.buf),
        KeyEvent::Char('g')                                       => { state.normal_accum.pending_g = true; return StepResult::Continue; }
        KeyEvent::Char('w')                                       => state.buf.cursor = motion::word_forward(&mut state.buf, count),
        KeyEvent::Char('b')                                       => state.buf.cursor = motion::word_backward(&mut state.buf, count),
        KeyEvent::Char('%')                                       => state.buf.cursor = motion::match_bracket(&mut state.buf),
        KeyEvent::Char('i')                                       => { state.mode = Mode::Insert; }
        KeyEvent::Char('a')                                       => { state.buf.cursor = motion::right(&mut state.buf, 1); state.mode = Mode::Insert; }
        KeyEvent::Char('o')                                       => {
            state.buf.cursor = motion::line_end(&mut state.buf);
            state.buf.pieces.insert(state.buf.cursor, b"\n");
            state.buf.cursor += 1;
            state.buf.mark_dirty();
            state.mode = Mode::Insert;
        }
        KeyEvent::Char(':')                                       => { state.mode = Mode::ExPrompt(PromptKind::Ex); }
        KeyEvent::Char('v') => { state.visual_anchor = state.buf.cursor; state.mode = Mode::VisualChar; }
        KeyEvent::Char('V') => { state.visual_anchor = state.buf.cursor; state.mode = Mode::VisualLine; }
        KeyEvent::Char('d') => { state.mode = Mode::OperatorPending(Operator::Delete); return StepResult::Continue; }
        KeyEvent::Char('y') => { state.mode = Mode::OperatorPending(Operator::Yank);   return StepResult::Continue; }
        KeyEvent::Char('c') => { state.mode = Mode::OperatorPending(Operator::Change); return StepResult::Continue; }
        KeyEvent::Char('>') => { state.mode = Mode::OperatorPending(Operator::Indent); return StepResult::Continue; }
        KeyEvent::Char('<') => { state.mode = Mode::OperatorPending(Operator::Dedent); return StepResult::Continue; }
        KeyEvent::Char('x') => { ops::delete_char(state, count); }
        KeyEvent::Char('p') => { ops::paste_after(state); }
        KeyEvent::Char('u') => { if let Some(c) = state.undo.undo(&mut state.buf.pieces) { state.buf.cursor = c; } }
        KeyEvent::Ctrl('r') => { if let Some(c) = state.undo.redo(&mut state.buf.pieces) { state.buf.cursor = c; } }
        KeyEvent::Ctrl('q')                                       => return StepResult::Quit(0),
        KeyEvent::Ctrl('s')                                       => { /* :w shortcut, lands in Task 31 */ }
        _                                                          => return StepResult::Continue,
    }
    StepResult::Redraw
}
