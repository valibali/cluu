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
}

impl PromptState {
    pub fn new(kind: PromptKind) -> Self {
        PromptState { buf: String::new(), kind }
    }
}

pub fn handle(state: &mut Editor, event: KeyEvent, kind: PromptKind) -> StepResult {
    let prompt = state.prompt.get_or_insert_with(|| PromptState::new(kind));
    match event {
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
