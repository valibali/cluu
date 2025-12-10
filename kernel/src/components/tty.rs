/*
 * TTY Layer
 *
 * Provides a terminal abstraction on top of:
 *  - framebuffer console (for output)
 *  - LineEditor (for canonical input and history)
 *
 * For now we have only one TTY: TTY0 = the GRID console.
 */

use crate::utils::io::console::{self, Color};
use crate::utils::ui::line_editor::LineEditor;
use alloc::string::String;
use spin::Mutex;

/// Canonical vs raw mode (future use)
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TtyInputMode {
    Canonical,
    Raw,
}

/// One TTY instance.
pub struct Tty {
    id: u8,
    mode: TtyInputMode,
    echo: bool,
    line_editor: LineEditor,
}

/// Completed line type coming from TTY.
pub type TtyLine = String;

/// Primary TTY (GRID).
pub static TTY0: Mutex<Option<Tty>> = Mutex::new(None);

impl Tty {
    pub fn new(id: u8) -> Self {
        Self {
            id,
            mode: TtyInputMode::Canonical,
            echo: true,
            line_editor: LineEditor::new(),
        }
    }

    /// Initialize TTY0 console backend.
    /// We keep prompt & banner logic in the shell.
    pub fn init(&mut self) {
        console::init();
        // Don't clear here; Shell will clear & print banner.
    }

    /// Handle a single input character.
    /// Returns Some(line) when a full line is ready.
    pub fn handle_input_char(&mut self, ch: char) -> Option<TtyLine> {
        match self.mode {
            TtyInputMode::Canonical => self.line_editor.handle_char(ch),
            TtyInputMode::Raw => {
                if self.echo {
                    console::write_char(ch);
                }
                let mut s = String::new();
                s.push(ch);
                Some(s)
            }
        }
    }

    pub fn set_mode(&mut self, mode: TtyInputMode) {
        self.mode = mode;
    }

    pub fn mode(&self) -> TtyInputMode {
        self.mode
    }

    pub fn set_echo(&mut self, echo: bool) {
        self.echo = echo;
    }

    pub fn echo(&self) -> bool {
        self.echo
    }

    pub fn history(&self) -> &[String] {
        self.line_editor.get_history()
    }

    // Output helpers (just forward to console for now)
    pub fn write_str(&self, s: &str) {
        console::write_str(s);
    }

    pub fn write_line(&self, s: &str) {
        console::write_str(s);
        console::write_str("\n");
    }

    pub fn write_colored(&self, s: &str, fg: Color, bg: Color) {
        console::write_colored(s, fg, bg);
    }

    pub fn clear(&mut self) {
        console::clear_screen();
    }
}

/// Initialize TTY0 – call this from kstart before starting the shell.
pub fn init_tty0() {
    let mut guard = TTY0.lock();
    if guard.is_none() {
        let mut tty = Tty::new(0);
        tty.init();
        *guard = Some(tty);
    }
}

/// Pass an input character into TTY0.
/// Returns Some(line) when a full line is ready for the shell.
pub fn tty0_handle_char(ch: char) -> Option<TtyLine> {
    let mut guard = TTY0.lock();
    let tty = guard.as_mut().expect("TTY0 not initialized");
    tty.handle_input_char(ch)
}

/// Helper: run a closure with mutable access to TTY0.
/// Used e.g. from shell to access history safely.
pub fn with_tty0<F, R>(f: F) -> R
where
    F: FnOnce(&mut Tty) -> R,
{
    let mut guard = TTY0.lock();
    let tty = guard.as_mut().expect("TTY0 not initialized");
    f(tty)
}

/// Output helpers from outside:
pub fn tty0_write_str(s: &str) {
    with_tty0(|tty| tty.write_str(s));
}

pub fn tty0_write_line(s: &str) {
    with_tty0(|tty| tty.write_line(s));
}
