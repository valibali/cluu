//! NORMAL-mode keymap. Skeleton — h/j/k/l + i + :q only. Operators,
//! counted motions, gd, etc. land in later tasks.

use crate::input::{KeyEvent, Direction};
use crate::mode::{Editor, Mode, StepResult};

pub fn handle(state: &mut Editor, event: KeyEvent) -> StepResult {
    match event {
        KeyEvent::Char('h') | KeyEvent::Arrow(Direction::Left)  => motion_left(state),
        KeyEvent::Char('l') | KeyEvent::Arrow(Direction::Right) => motion_right(state),
        KeyEvent::Char('j') | KeyEvent::Arrow(Direction::Down)  => motion_down(state),
        KeyEvent::Char('k') | KeyEvent::Arrow(Direction::Up)    => motion_up(state),
        KeyEvent::Char('i') => { state.mode = Mode::Insert; StepResult::Redraw }
        KeyEvent::Char(':') => { state.mode = Mode::ExPrompt(crate::mode::PromptKind::Ex); StepResult::Redraw }
        // Quick-quit shortcut for the skeleton; real :q lands with the prompt.
        KeyEvent::Ctrl('q') => StepResult::Quit(0),
        _ => StepResult::Continue,
    }
}

fn motion_left(state: &mut Editor) -> StepResult {
    if state.buf.cursor > 0 { state.buf.cursor -= 1; }
    StepResult::Redraw
}

fn motion_right(state: &mut Editor) -> StepResult {
    if state.buf.cursor < state.buf.pieces.len() {
        state.buf.cursor += 1;
    }
    StepResult::Redraw
}

fn motion_down(state: &mut Editor) -> StepResult {
    let (line, col) = state.buf.pieces.line_col(state.buf.cursor);
    let total_lines = state.buf.pieces.line_count();
    if line + 1 >= total_lines { return StepResult::Continue; }
    let idx = state.buf.pieces.line_index().to_vec();
    let next_start = idx[line + 1];
    let next_end = if line + 2 < idx.len() { idx[line + 2] - 1 } else { state.buf.pieces.len() };
    let new_col = col.min(next_end - next_start);
    state.buf.cursor = next_start + new_col;
    StepResult::Redraw
}

fn motion_up(state: &mut Editor) -> StepResult {
    let (line, col) = state.buf.pieces.line_col(state.buf.cursor);
    if line == 0 { return StepResult::Continue; }
    let idx = state.buf.pieces.line_index().to_vec();
    let prev_start = idx[line - 1];
    let prev_end = idx[line] - 1;
    let new_col = col.min(prev_end - prev_start);
    state.buf.cursor = prev_start + new_col;
    StepResult::Redraw
}
