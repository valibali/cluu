//! Line discipline for TTY input.
//!
//! This buffers characters until a line terminator, echoes keystrokes to the
//! console, and delivers complete lines to the shell.

extern crate alloc;

use alloc::vec::Vec;

const BACKSPACE_SEQ: &[u8] = b"\x08 \x08";

/// Output of processing a single input byte.
pub struct LineEffect {
    pub echo: EchoAction,
    pub line_ready: Option<Vec<u8>>,
    /// In raw mode, each byte is delivered immediately (no line buffering).
    pub raw_byte: Option<u8>,
}

/// Echo action for the console.
pub enum EchoAction {
    None,
    Bytes(&'static [u8]),
    Byte(u8),
}

/// Terminal mode flags (subset of POSIX termios c_lflag).
#[derive(Clone, Copy)]
pub struct TermMode {
    /// ICANON: canonical (line-buffered) mode.
    pub canonical: bool,
    /// ECHO: echo input characters to console.
    pub echo: bool,
}

impl Default for TermMode {
    fn default() -> Self {
        Self {
            canonical: true,
            echo: true,
        }
    }
}

/// Line discipline with configurable mode.
///
/// In canonical mode it buffers input and only emits a line when Enter is pressed.
/// In raw mode each byte is delivered immediately with optional echo.
pub struct LineDiscipline {
    buffer: Vec<u8>,
    pub mode: TermMode,
}

impl LineDiscipline {
    /// Create a new line discipline in canonical+echo mode.
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            mode: TermMode::default(),
        }
    }

    /// Update the terminal mode.
    pub fn set_mode(&mut self, mode: TermMode) {
        self.mode = mode;
        // When switching to raw mode, flush any buffered canonical input
        if !mode.canonical && !self.buffer.is_empty() {
            self.buffer.clear();
        }
    }

    /// Process a byte and return echo/line delivery actions.
    pub fn handle_byte(&mut self, byte: u8) -> LineEffect {
        if !self.mode.canonical {
            return self.handle_byte_raw(byte);
        }
        self.handle_byte_canonical(byte)
    }

    /// Canonical mode: buffer input, emit line on Enter.
    fn handle_byte_canonical(&mut self, byte: u8) -> LineEffect {
        match byte {
            0x03 => {
                // Ctrl-C (SIGINT): clear current line buffer and forward an out-of-band
                // marker byte so foreground consumers can interrupt promptly.
                self.buffer.clear();
                LineEffect {
                    echo: if self.mode.echo {
                        EchoAction::Bytes(b"^C\n")
                    } else {
                        EchoAction::None
                    },
                    line_ready: Some(alloc::vec![0x03]),
                    raw_byte: None,
                }
            }
            0x04 => {
                // Ctrl-D (EOT): forward directly so foreground REPLs can exit on EOF.
                // If there is buffered input, flush that partial line first.
                if self.buffer.is_empty() {
                    LineEffect {
                        echo: EchoAction::None,
                        line_ready: Some(alloc::vec![0x04]),
                        raw_byte: None,
                    }
                } else {
                    let line = core::mem::take(&mut self.buffer);
                    LineEffect {
                        echo: EchoAction::None,
                        line_ready: Some(line),
                        raw_byte: None,
                    }
                }
            }
            b'\n' => {
                self.buffer.push(byte);
                let line = core::mem::take(&mut self.buffer);
                LineEffect {
                    echo: if self.mode.echo {
                        EchoAction::Bytes(b"\n")
                    } else {
                        EchoAction::None
                    },
                    line_ready: Some(line),
                    raw_byte: None,
                }
            }
            0x08 => {
                if !self.buffer.is_empty() {
                    self.buffer.pop();
                    LineEffect {
                        echo: if self.mode.echo {
                            EchoAction::Bytes(BACKSPACE_SEQ)
                        } else {
                            EchoAction::None
                        },
                        line_ready: None,
                        raw_byte: None,
                    }
                } else {
                    LineEffect {
                        echo: EchoAction::None,
                        line_ready: None,
                        raw_byte: None,
                    }
                }
            }
            _ => {
                self.buffer.push(byte);
                LineEffect {
                    echo: if self.mode.echo {
                        EchoAction::Byte(byte)
                    } else {
                        EchoAction::None
                    },
                    line_ready: None,
                    raw_byte: None,
                }
            }
        }
    }

    /// Raw mode: deliver each byte immediately, optional echo.
    fn handle_byte_raw(&self, byte: u8) -> LineEffect {
        LineEffect {
            echo: if self.mode.echo {
                EchoAction::Byte(byte)
            } else {
                EchoAction::None
            },
            line_ready: None,
            raw_byte: Some(byte),
        }
    }
}
