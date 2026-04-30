//! Mode state machine (top-level dispatch). See spec §5.

extern crate alloc;
use alloc::string::String;
use crate::buffer::EditBuffer;
use crate::undo::UndoStack;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    VisualChar,
    VisualLine,
    OperatorPending(Operator),
    ExPrompt(PromptKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator { Delete, Change, Yank, Indent, Dedent }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind { Ex, SearchFwd, SearchBwd }

#[derive(Debug)]
pub enum StepResult {
    Continue,
    Redraw,
    Quit(i32),
}

/// Editor viewport — the rectangular slice of the buffer currently visible on
/// screen. `top_line`/`left_col` track the scroll origin (0-indexed); `height`
/// is the number of content rows (status + message rows live below);
/// `width` is total terminal columns. Default is the conservative 80x24
/// console layout (22 content rows + 1 status + 1 message).
#[allow(dead_code)]
pub struct Viewport {
    pub top_line: usize,
    pub left_col: usize,
    pub height: u16,
    pub width: u16,
}

impl Viewport {
    pub fn default_80x24() -> Self {
        Viewport { top_line: 0, left_col: 0, height: 22, width: 80 }
    }
}

pub struct Editor {
    pub buf: EditBuffer,
    pub undo: UndoStack,
    pub mode: Mode,
    pub running: bool,
    pub message: String,
    pub prompt: Option<crate::prompt::PromptState>,
    pub viewport: Viewport,
    // More fields added in later tasks (settings, search state, etc.)
}

impl Editor {
    pub fn new(buf: EditBuffer) -> Self {
        Editor {
            buf,
            undo: UndoStack::new(),
            mode: Mode::Normal,
            running: true,
            message: String::new(),
            prompt: None,
            viewport: Viewport::default_80x24(),
        }
    }
}

use crate::input::KeyEvent;

pub fn handle(state: &mut Editor, event: KeyEvent) -> StepResult {
    match state.mode {
        Mode::Normal => crate::normal::handle(state, event),
        Mode::Insert => crate::insert::handle(state, event),
        Mode::ExPrompt(kind) => crate::prompt::handle(state, event, kind),
        // Other modes added in later tasks. For the skeleton, fall through
        // to NORMAL to avoid wedging.
        _ => crate::normal::handle(state, event),
    }
}
