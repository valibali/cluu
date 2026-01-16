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
}

/// Echo action for the console.
pub enum EchoAction {
    None,
    Bytes(&'static [u8]),
    Byte(u8),
}

/// Simple canonical line discipline.
///
/// It buffers input and only emits a line when Enter is pressed.
pub struct LineDiscipline {
    buffer: Vec<u8>,
}

impl LineDiscipline {
    /// Create a new line discipline with an empty buffer.
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Process a byte and return echo/line delivery actions.
    pub fn handle_byte(&mut self, byte: u8) -> LineEffect {
        match byte {
            b'\n' => {
                self.buffer.push(byte);
                let line = core::mem::take(&mut self.buffer);
                LineEffect {
                    echo: EchoAction::Bytes(b"\n"),
                    line_ready: Some(line),
                }
            }
            0x08 => {
                if !self.buffer.is_empty() {
                    self.buffer.pop();
                    LineEffect {
                        echo: EchoAction::Bytes(BACKSPACE_SEQ),
                        line_ready: None,
                    }
                } else {
                    LineEffect {
                        echo: EchoAction::None,
                        line_ready: None,
                    }
                }
            }
            _ => {
                self.buffer.push(byte);
                LineEffect {
                    echo: EchoAction::Byte(byte),
                    line_ready: None,
                }
            }
        }
    }
}
