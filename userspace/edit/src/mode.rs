//! Mode state machine (top-level dispatch). See spec §5.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
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

pub struct Settings {
    pub expandtab: bool,
    pub tabstop: u8,
    pub smartindent: bool,
    pub ignorecase: bool,
    pub hlsearch: bool,
    pub wrap: bool,
    pub number: bool,
    pub scrolloff: u8,
}

impl Settings {
    pub fn defaults() -> Self {
        Settings {
            expandtab: false,
            tabstop: 4,
            smartindent: true,
            ignorecase: false,
            hlsearch: true,
            wrap: false,
            number: false,
            scrolloff: 3,
        }
    }
}

pub struct SearchState {
    pub pattern: String,
    pub direction: SearchDir,
    pub matches: Vec<core::ops::Range<usize>>,
    pub matches_seq: u64,
    pub history: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchDir { Forward, Backward }

impl SearchState {
    pub fn new() -> Self {
        SearchState {
            pattern: String::new(),
            direction: SearchDir::Forward,
            matches: Vec::new(),
            matches_seq: u64::MAX,
            history: Vec::new(),
        }
    }
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

    /// Query the actual console size from the boot framebuffer info and reserve
    /// 2 rows for the status + message lines. Falls back to 80×24 if the
    /// framebuffer dimensions aren't available (host has no FB).
    pub fn from_console() -> Self {
        const GLYPH_W: u32 = 8;
        const GLYPH_H: u32 = 16;
        let info = libcluu::boot::boot_info();
        if info.fb_width == 0 || info.fb_height == 0 {
            return Self::default_80x24();
        }
        let cols = info.fb_width / GLYPH_W;
        let rows = info.fb_height / GLYPH_H;
        let content_rows = rows.saturating_sub(2);
        Viewport {
            top_line: 0,
            left_col: 0,
            height: content_rows.min(u16::MAX as u32) as u16,
            width: cols.min(u16::MAX as u32) as u16,
        }
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
    pub normal_accum: crate::normal::NormalAccum,
    pub register: Vec<u8>,
    pub visual_anchor: usize,
    pub last_visual_range: Option<(usize, usize, Mode)>,
    pub settings: Settings,
    pub search: SearchState,
    pub awaiting_replace: bool,
    pub ex_history: alloc::vec::Vec<alloc::string::String>,
    pub search_history: alloc::vec::Vec<alloc::string::String>,
    // More fields added in later tasks (search state, etc.)
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
            viewport: Viewport::from_console(),
            normal_accum: crate::normal::NormalAccum::new(),
            register: Vec::new(),
            visual_anchor: 0,
            last_visual_range: None,
            settings: Settings::defaults(),
            search: SearchState::new(),
            awaiting_replace: false,
            ex_history: alloc::vec::Vec::new(),
            search_history: alloc::vec::Vec::new(),
        }
    }
}

use crate::input::KeyEvent;

pub fn handle(state: &mut Editor, event: KeyEvent) -> StepResult {
    match state.mode {
        Mode::Normal             => crate::normal::handle(state, event),
        Mode::Insert             => crate::insert::handle(state, event),
        Mode::ExPrompt(kind)     => crate::prompt::handle(state, event, kind),
        Mode::OperatorPending(op) => crate::op_pending::handle(state, event, op),
        Mode::VisualChar | Mode::VisualLine => crate::visual::handle(state, event),
    }
}
