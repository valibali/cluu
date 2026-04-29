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

pub struct Editor {
    pub buf: EditBuffer,
    pub undo: UndoStack,
    pub mode: Mode,
    pub running: bool,
    pub message: String,
    pub prompt: Option<crate::prompt::PromptState>,
    // More fields added in later tasks (settings, search state, viewport, etc.)
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
