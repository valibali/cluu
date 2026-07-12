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

    /// Query the actual console size from `ProcessInfo.params` (procmgr injects
    /// `PARAM_FB_WIDTH`/`PARAM_FB_HEIGHT` into every spawned child) and
    /// reserve 2 rows for the status + message lines. Falls back to 80×24 if
    /// the params are zero — `boot_info()` only works for init, not children.
    pub fn from_console() -> Self {
        extern "C" {
            fn _ioctl(fd: i32, request: usize, argp: *mut core::ffi::c_void) -> i32;
        }
        #[repr(C)]
        struct WinSize { ws_row: u16, ws_col: u16, ws_xpixel: u16, ws_ypixel: u16 }
        const TIOCGWINSZ: usize = 0x5413;
        let mut ws = WinSize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
        let rc = unsafe { _ioctl(1, TIOCGWINSZ, &mut ws as *mut _ as *mut core::ffi::c_void) };
        if rc == 0 && ws.ws_col > 0 {
            let cols = ws.ws_col as u64;
            let rows = ws.ws_row as u64;
            let content_rows = rows.saturating_sub(2);
            return Viewport {
                top_line: 0, left_col: 0,
                height: content_rows.min(u16::MAX as u64) as u16,
                width: cols.min(u16::MAX as u64) as u16,
            };
        }
        Self::default_80x24()
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
    pub plugin_ex_command: Option<alloc::string::String>,
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
            plugin_ex_command: None,
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
