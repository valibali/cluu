//! libtui — Elm-style MVU (Model-View-Update) runtime for CLUU TUI apps.
//!
//! Provides:
//! - `Model` trait: init/update/view interface for TUI applications
//! - `Cmd` type: lazily-evaluated side-effect descriptor
//! - `View` / `Cell`: renderable grid of styled characters
//! - Input decoder: raw TTY bytes -> `KeyEvent`
//! - `Program`: event loop (raw mode, alt-screen, read/update/render)
//!
//! no_std + alloc. Runtime parts (StdinReader, Renderer, Program) depend
//! on libcluu for TTY IPC and POSIX I/O.

#![no_std]
extern crate alloc;

pub mod input;
pub mod render;
pub mod diff;
pub mod components;
pub mod style;
#[cfg(feature = "runtime")]
pub mod program;

use alloc::vec::Vec;

// =========================================================================
// Model trait — Elm MVU
// =========================================================================

/// Elm-style Model trait. Each application implements this with its own
/// `Msg` associated type. The runtime calls `init` once, then loops
/// `update -> view -> render`.
pub trait Model: Sized {
    /// Application-specific message type (typically an enum).
    type Msg;

    /// Initial model state and startup command.
    fn init() -> (Self, Cmd);

    /// Process a message, mutating state. Returns a `Cmd` describing
    /// follow-up effects.
    fn update(&mut self, msg: Self::Msg) -> Cmd;

    /// Render the current state as a `View`.
    fn view(&self) -> View;

    /// Convert a key event into a message. Return `None` to ignore.
    fn from_key(key: input::KeyEvent) -> Option<Self::Msg>;

    /// Optional terminal cursor position after rendering (0-indexed row,
    /// col). The Program emits `CSI row+1;col+1 H` after the diff render.
    /// Default: `None` (no cursor positioning).
    fn cursor_position(&self) -> Option<(usize, usize)> {
        None
    }

    fn on_resize(&mut self) {}
}

// =========================================================================
// Cmd — side-effect descriptor
// =========================================================================

/// A lazily-evaluated side-effect descriptor.
///
/// For v0, `None` and `Quit` are the only variants the `Program` runtime
/// acts on. `Batch` and `Sequence` are structural — they compose cmds for
/// future execution but the v0 runtime flattens them without performing
/// I/O or async work. Cmd is non-generic because v0 cmds do not carry
/// Msg-producing effects; a future revision may parameterize it.
pub enum Cmd {
    /// No side-effect.
    None,
    /// Run all cmds concurrently (structural — v0 flattens to no-op).
    Batch(Vec<Cmd>),
    /// Run cmds in order, stopping on first that produces a msg
    /// (structural — v0 flattens to no-op).
    Sequence(Vec<Cmd>),
    /// Signal the program loop to exit.
    Quit,
}

impl Cmd {
    pub fn none() -> Self {
        Cmd::None
    }

    pub fn quit() -> Self {
        Cmd::Quit
    }

    pub fn batch(cmds: Vec<Cmd>) -> Self {
        Cmd::Batch(cmds)
    }

    pub fn sequence(cmds: Vec<Cmd>) -> Self {
        Cmd::Sequence(cmds)
    }

    /// Returns true if this cmd (or any nested cmd) signals quit.
    pub fn should_quit(&self) -> bool {
        match self {
            Cmd::Quit => true,
            Cmd::Batch(cmds) | Cmd::Sequence(cmds) => cmds.iter().any(|c| c.should_quit()),
            Cmd::None => false,
        }
    }
}

impl Default for Cmd {
    fn default() -> Self {
        Cmd::None
    }
}

// =========================================================================
// View — renderable cell grid
// =========================================================================

/// SGR foreground/background color codes. 0 = default.
pub const COLOR_DEFAULT: u8 = 0;
pub const COLOR_BLACK: u8 = 0;
pub const COLOR_RED: u8 = 1;
pub const COLOR_GREEN: u8 = 2;
pub const COLOR_YELLOW: u8 = 3;
pub const COLOR_BLUE: u8 = 4;
pub const COLOR_MAGENTA: u8 = 5;
pub const COLOR_CYAN: u8 = 6;
pub const COLOR_WHITE: u8 = 7;

/// Cell attributes bitmask.
pub const ATTR_NONE: u8 = 0;
pub const ATTR_BOLD: u8 = 1;
pub const ATTR_UNDERLINE: u8 = 2;
pub const ATTR_REVERSE: u8 = 4;

/// A single styled cell in the view grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: u8,
    pub bg: u8,
    pub attrs: u8,
}

impl Cell {
    pub fn new(ch: char) -> Self {
        Cell { ch, fg: COLOR_DEFAULT, bg: COLOR_DEFAULT, attrs: ATTR_NONE }
    }

    pub fn fg(mut self, fg: u8) -> Self {
        self.fg = fg;
        self
    }

    pub fn bg(mut self, bg: u8) -> Self {
        self.bg = bg;
        self
    }

    pub fn attrs(mut self, attrs: u8) -> Self {
        self.attrs = attrs;
        self
    }
}

/// A renderable grid of cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub cells: Vec<Cell>,
    pub width: usize,
    pub height: usize,
}

impl View {
    /// Create a blank view filled with spaces.
    pub fn new(width: usize, height: usize) -> Self {
        let cells = alloc::vec![Cell::new(' '); width * height];
        View { cells, width, height }
    }

    /// Get the cell at (row, col). Returns None if out of bounds.
    pub fn get(&self, row: usize, col: usize) -> Option<&Cell> {
        if row < self.height && col < self.width {
            self.cells.get(row * self.width + col)
        } else {
            None
        }
    }

    /// Set the cell at (row, col). No-op if out of bounds.
    pub fn set(&mut self, row: usize, col: usize, cell: Cell) {
        if row < self.height && col < self.width {
            self.cells[row * self.width + col] = cell;
        }
    }

    /// Fill the entire view with a single cell.
    pub fn fill(&mut self, cell: Cell) {
        for c in &mut self.cells {
            *c = cell;
        }
    }

    /// Write a string at (row, col), clipping to view bounds.
    pub fn write_str(&mut self, row: usize, col: usize, s: &str) {
        let mut c = col;
        for ch in s.chars() {
            if c >= self.width || row >= self.height {
                break;
            }
            self.set(row, c, Cell::new(ch));
            c += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyEvent;
    use alloc::string::String;
    use alloc::vec;

    // --- Cmd tests ---

    #[test]
    fn cmd_none_should_not_quit() {
        let cmd = Cmd::none();
        assert!(!cmd.should_quit());
    }

    #[test]
    fn cmd_quit_should_quit() {
        let cmd = Cmd::quit();
        assert!(cmd.should_quit());
    }

    #[test]
    fn cmd_batch_with_quit_should_quit() {
        let cmd = Cmd::batch(vec![Cmd::none(), Cmd::quit(), Cmd::none()]);
        assert!(cmd.should_quit());
    }

    #[test]
    fn cmd_batch_without_quit_should_not_quit() {
        let cmd = Cmd::batch(vec![Cmd::none(), Cmd::none()]);
        assert!(!cmd.should_quit());
    }

    #[test]
    fn cmd_sequence_with_quit_should_quit() {
        let cmd = Cmd::sequence(vec![Cmd::none(), Cmd::quit()]);
        assert!(cmd.should_quit());
    }

    #[test]
    fn cmd_nested_batch_quit_should_quit() {
        let inner = Cmd::batch(vec![Cmd::quit()]);
        let outer = Cmd::batch(vec![Cmd::none(), inner]);
        assert!(outer.should_quit());
    }

    #[test]
    fn cmd_default_is_none() {
        let cmd = Cmd::default();
        assert!(!cmd.should_quit());
    }

    // --- View/Cell tests ---

    #[test]
    fn view_new_filled_with_spaces() {
        let v = View::new(3, 2);
        assert_eq!(v.width, 3);
        assert_eq!(v.height, 2);
        assert_eq!(v.cells.len(), 6);
        for cell in &v.cells {
            assert_eq!(cell.ch, ' ');
        }
    }

    #[test]
    fn view_set_and_get() {
        let mut v = View::new(3, 2);
        v.set(0, 1, Cell::new('X'));
        assert_eq!(v.get(0, 1).map(|c| c.ch), Some('X'));
        assert_eq!(v.get(0, 0).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn view_set_out_of_bounds_is_noop() {
        let mut v = View::new(2, 2);
        v.set(5, 5, Cell::new('Z'));
        assert_eq!(v.cells.len(), 4);
    }

    #[test]
    fn view_write_str_clips() {
        let mut v = View::new(3, 1);
        v.write_str(0, 0, "hello");
        assert_eq!(v.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(v.get(0, 1).map(|c| c.ch), Some('e'));
        assert_eq!(v.get(0, 2).map(|c| c.ch), Some('l'));
    }

    #[test]
    fn cell_builder_methods() {
        let c = Cell::new('A').fg(COLOR_RED).bg(COLOR_WHITE).attrs(ATTR_BOLD);
        assert_eq!(c.ch, 'A');
        assert_eq!(c.fg, COLOR_RED);
        assert_eq!(c.bg, COLOR_WHITE);
        assert_eq!(c.attrs, ATTR_BOLD);
    }

    // --- Minimal Model test ---

    struct CounterModel {
        count: u32,
    }

    #[allow(dead_code)]
    enum CounterMsg {
        Key(KeyEvent),
        Quit,
    }

    impl Model for CounterModel {
        type Msg = CounterMsg;

        fn init() -> (Self, Cmd) {
            (CounterModel { count: 0 }, Cmd::none())
        }

        fn update(&mut self, msg: CounterMsg) -> Cmd {
            match msg {
                CounterMsg::Key(_) => {
                    self.count += 1;
                    Cmd::none()
                }
                CounterMsg::Quit => Cmd::quit(),
            }
        }

        fn view(&self) -> View {
            let mut v = View::new(20, 1);
            let s = alloc::format!("count: {}", self.count);
            v.write_str(0, 0, &s);
            v
        }

        fn from_key(key: KeyEvent) -> Option<CounterMsg> {
            match key {
                KeyEvent::Ctrl('c') => Some(CounterMsg::Quit),
                other => Some(CounterMsg::Key(other)),
            }
        }
    }

    #[test]
    fn counter_model_init_and_update() {
        let (mut model, cmd) = CounterModel::init();
        assert!(!cmd.should_quit());
        assert_eq!(model.count, 0);

        let key = KeyEvent::Char('a');
        let msg = CounterModel::from_key(key).unwrap();
        let cmd = model.update(msg);
        assert!(!cmd.should_quit());
        assert_eq!(model.count, 1);

        let msg = CounterModel::from_key(KeyEvent::Char('b')).unwrap();
        model.update(msg);
        assert_eq!(model.count, 2);
    }

    #[test]
    fn counter_model_quit_on_ctrl_c() {
        let (mut model, _) = CounterModel::init();
        let msg = CounterModel::from_key(KeyEvent::Ctrl('c')).unwrap();
        let cmd = model.update(msg);
        assert!(cmd.should_quit());
    }

    #[test]
    fn counter_model_view_shows_count() {
        let (mut model, _) = CounterModel::init();
        model.update(CounterModel::from_key(KeyEvent::Char('x')).unwrap());
        model.update(CounterModel::from_key(KeyEvent::Char('y')).unwrap());
        let v = model.view();
        assert_eq!(v.get(0, 0).map(|c| c.ch), Some('c'));
        let cell_str: String = v.cells.iter().take(9).map(|c| c.ch).collect();
        assert_eq!(cell_str, "count: 2 ");
    }
}
