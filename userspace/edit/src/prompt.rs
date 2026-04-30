//! Ex / search prompt buffer + minimal command dispatch.
//! Full ex parser lands in Task 26. For now: :q, :q!, :wq.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use crate::input::KeyEvent;
use crate::mode::{Editor, Mode, PromptKind, StepResult};

pub struct PromptState {
    pub buf: String,
    pub kind: PromptKind,
    pub history_idx: Option<usize>,
}

impl PromptState {
    pub fn new(kind: PromptKind) -> Self {
        PromptState { buf: String::new(), kind, history_idx: None }
    }
}

pub fn handle(state: &mut Editor, event: KeyEvent, kind: PromptKind) -> StepResult {
    let prompt = state.prompt.get_or_insert_with(|| PromptState::new(kind));
    match event {
        KeyEvent::Arrow(crate::input::Direction::Up) => {
            let history = match prompt.kind {
                PromptKind::Ex => &state.ex_history,
                _              => &state.search_history,
            };
            if !history.is_empty() {
                let next = match prompt.history_idx {
                    None => history.len() - 1,
                    Some(0) => 0,
                    Some(i) => i - 1,
                };
                prompt.history_idx = Some(next);
                prompt.buf = history[next].clone();
            }
            StepResult::Redraw
        }
        KeyEvent::Arrow(crate::input::Direction::Down) => {
            let history = match prompt.kind {
                PromptKind::Ex => &state.ex_history,
                _              => &state.search_history,
            };
            if let Some(i) = prompt.history_idx {
                if i + 1 < history.len() {
                    prompt.history_idx = Some(i + 1);
                    prompt.buf = history[i + 1].clone();
                } else {
                    prompt.history_idx = None;
                    prompt.buf.clear();
                }
            }
            StepResult::Redraw
        }
        KeyEvent::Esc => {
            state.prompt = None;
            state.mode = Mode::Normal;
            StepResult::Redraw
        }
        KeyEvent::Enter => {
            let line = core::mem::take(&mut prompt.buf);
            let kind = prompt.kind;
            state.prompt = None;
            state.mode = Mode::Normal;
            const HISTORY_MAX: usize = 50;
            match kind {
                PromptKind::Ex => {
                    state.ex_history.push(line.clone());
                    if state.ex_history.len() > HISTORY_MAX { state.ex_history.remove(0); }
                }
                _ => {
                    state.search_history.push(line.clone());
                    if state.search_history.len() > HISTORY_MAX { state.search_history.remove(0); }
                }
            }
            match kind {
                PromptKind::Ex => dispatch_ex(state, &line),
                PromptKind::SearchFwd => {
                    crate::search::set_pattern(state, line, crate::mode::SearchDir::Forward);
                    if let Some(p) = crate::search::next_match(state) { state.buf.cursor = p; }
                }
                PromptKind::SearchBwd => {
                    crate::search::set_pattern(state, line, crate::mode::SearchDir::Backward);
                    if let Some(p) = crate::search::next_match(state) { state.buf.cursor = p; }
                }
            }
            StepResult::Redraw
        }
        KeyEvent::Backspace => {
            prompt.buf.pop();
            StepResult::Redraw
        }
        KeyEvent::Char(c) => {
            prompt.buf.push(c);
            StepResult::Redraw
        }
        _ => StepResult::Continue,
    }
}

fn dispatch_ex(state: &mut Editor, line: &str) {
    crate::ex::dispatch(state, line);
}
