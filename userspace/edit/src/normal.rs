//! NORMAL-mode keymap with count accumulator and full motion set.
//! See spec §7.2 + §7.3.

extern crate alloc;

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
    // Awaiting replace (r{c}) — clear flag FIRST so Esc/non-Char doesn't stick.
    if state.awaiting_replace {
        state.awaiting_replace = false;
        if let KeyEvent::Char(c) = event {
            if state.buf.cursor < state.buf.pieces.len() {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                if let Some(patch) = state.buf.pieces.delete(state.buf.cursor..state.buf.cursor + 1) {
                    state.undo.record(state.buf.cursor, state.buf.cursor, patch);
                    state.buf.pieces.insert(state.buf.cursor, s.as_bytes());
                    state.buf.mark_dirty();
                }
            }
        }
        return StepResult::Redraw;
    }

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
        if let KeyEvent::Char('d') = event {
            let w = crate::search::word_at_cursor(state);
            if !w.is_empty() {
                for kw in &["fn", "struct", "let", "const", "enum", "impl", "mod", "trait", "type"] {
                    let pat = alloc::format!("{} {}", kw, w);
                    crate::search::set_pattern(state, pat, crate::mode::SearchDir::Backward);
                    if let Some(p) = crate::search::next_match(state) {
                        state.buf.cursor = p;
                        return StepResult::Redraw;
                    }
                }
                crate::search::set_pattern(state, w, crate::mode::SearchDir::Backward);
                if let Some(p) = crate::search::next_match(state) { state.buf.cursor = p; }
            }
            return StepResult::Redraw;
        }
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
        KeyEvent::Char('I') => {
            state.buf.cursor = motion::line_start(&mut state.buf);
            let bytes = state.buf.pieces.read_all();
            while state.buf.cursor < bytes.len() && (bytes[state.buf.cursor] == b' ' || bytes[state.buf.cursor] == b'\t') {
                state.buf.cursor += 1;
            }
            state.mode = Mode::Insert;
        }
        KeyEvent::Char('A') => {
            state.buf.cursor = motion::line_end(&mut state.buf);
            state.mode = Mode::Insert;
        }
        KeyEvent::Char('O') => {
            state.buf.cursor = motion::line_start(&mut state.buf);
            state.buf.pieces.insert(state.buf.cursor, b"\n");
            state.buf.mark_dirty();
            state.mode = Mode::Insert;
        }
        KeyEvent::Char('P') => {
            if !state.register.is_empty() {
                let bytes = state.register.clone();
                let pos = state.buf.cursor;
                if let Some(patch) = state.buf.pieces.insert(pos, &bytes) {
                    state.undo.record(state.buf.cursor, pos, patch);
                    state.buf.mark_dirty();
                }
            }
        }
        KeyEvent::Char('r') => { state.awaiting_replace = true; return StepResult::Continue; }
        KeyEvent::Char('*') => {
            let w = crate::search::word_at_cursor(state);
            if !w.is_empty() {
                crate::search::set_pattern(state, w, crate::mode::SearchDir::Forward);
                if let Some(p) = crate::search::next_match(state) { state.buf.cursor = p; }
            }
        }
        KeyEvent::Char('#') => {
            let w = crate::search::word_at_cursor(state);
            if !w.is_empty() {
                crate::search::set_pattern(state, w, crate::mode::SearchDir::Backward);
                if let Some(p) = crate::search::next_match(state) { state.buf.cursor = p; }
            }
        }
        KeyEvent::Char(':')                                       => { state.mode = Mode::ExPrompt(PromptKind::Ex); }
        KeyEvent::Char('/') => { state.mode = Mode::ExPrompt(PromptKind::SearchFwd); }
        KeyEvent::Char('?') => { state.mode = Mode::ExPrompt(PromptKind::SearchBwd); }
        KeyEvent::Char('n') => { if let Some(p) = crate::search::next_match(state) { state.buf.cursor = p; } }
        KeyEvent::Char('N') => { if let Some(p) = crate::search::prev_match(state) { state.buf.cursor = p; } }
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
        KeyEvent::Ctrl('q') => { crate::ex::dispatch(state, "q"); }
        KeyEvent::Ctrl('s') => { crate::ex::dispatch(state, "w"); }
        _                                                          => return StepResult::Continue,
    }
    StepResult::Redraw
}
