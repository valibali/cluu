//! Line discipline for TTY input.
//!
//! This buffers characters until a line terminator, echoes keystrokes to the
//! console, and delivers complete lines to the shell.
//!
//! In canonical mode it also handles in-line editing: backspace deletes,
//! ↑/↓ recall command history, ←/→ are silently consumed (no mid-line cursor
//! editing yet — that lives in a future polish task).

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

const BACKSPACE_SEQ: &[u8] = b"\x08 \x08";
const HISTORY_CAP: usize = 32;

/// Output of processing a single input byte.
pub struct LineEffect {
    pub echo: EchoAction,
    pub line_ready: Option<Vec<u8>>,
    /// In raw mode, each byte is delivered immediately (no line buffering).
    pub raw_byte: Option<u8>,
    /// When TAB was pressed in canonical mode, this carries a snapshot of the
    /// current line buffer so the TTY main loop can run completion logic.
    /// `None` for any non-TAB byte.
    pub tab_request: Option<Vec<u8>>,
}

/// Echo action for the console.
pub enum EchoAction {
    None,
    Bytes(&'static [u8]),
    Byte(u8),
    /// Variable-length echo (used for redrawing the line on history recall).
    OwnedBytes(Vec<u8>),
}

/// State of the CSI escape sequence parser. CSI = `ESC [ ... <final-byte>`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CsiState {
    Idle,
    Esc,
    Bracket,
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
    /// In-memory command history (most recent at the back).
    history: VecDeque<Vec<u8>>,
    /// Current navigation index into history. `None` = user is editing a fresh
    /// line (or has navigated back past all history).
    history_pos: Option<usize>,
    /// User's partial input saved when they first hit ↑. Restored on ↓ past
    /// the most recent history entry.
    saved_partial: Option<Vec<u8>>,
    /// CSI escape-sequence parser state.
    csi_state: CsiState,
}

impl LineDiscipline {
    /// Create a new line discipline in canonical+echo mode.
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            mode: TermMode::default(),
            history: VecDeque::new(),
            history_pos: None,
            saved_partial: None,
            csi_state: CsiState::Idle,
        }
    }

    /// Update the terminal mode.
    pub fn set_mode(&mut self, mode: TermMode) {
        self.mode = mode;
        // When switching to raw mode, flush any buffered canonical input
        if !mode.canonical && !self.buffer.is_empty() {
            self.buffer.clear();
        }
        self.csi_state = CsiState::Idle;
    }

    /// Process a byte and return echo/line delivery actions.
    pub fn handle_byte(&mut self, byte: u8) -> LineEffect {
        if !self.mode.canonical {
            return self.handle_byte_raw(byte);
        }
        self.handle_byte_canonical(byte)
    }

    /// Build a sequence of bytes that visually erases the current buffer
    /// (BS-space-BS for each char) and writes the new content. Used for
    /// history recall (↑/↓).
    fn redraw_replacement(&self, new_buf: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.buffer.len() * 3 + new_buf.len());
        for _ in 0..self.buffer.len() {
            out.extend_from_slice(BACKSPACE_SEQ);
        }
        out.extend_from_slice(new_buf);
        out
    }

    /// Replace the input buffer from the history at `pos`. Emits visual redraw.
    /// Returns echo bytes to send.
    fn navigate_to_history(&mut self, pos: usize) -> Vec<u8> {
        let new_buf = self.history.get(pos).cloned().unwrap_or_default();
        let echo = self.redraw_replacement(&new_buf);
        self.buffer = new_buf;
        self.history_pos = Some(pos);
        echo
    }

    /// Restore the user's partial input (called when ↓ navigates past the
    /// newest history entry). Emits visual redraw.
    fn restore_partial(&mut self) -> Vec<u8> {
        let new_buf = self.saved_partial.take().unwrap_or_default();
        let echo = self.redraw_replacement(&new_buf);
        self.buffer = new_buf;
        self.history_pos = None;
        echo
    }

    /// Push a completed line into history (skipping duplicates of the most
    /// recent entry and empty lines). Drops oldest if at capacity.
    fn push_history(&mut self, line: &[u8]) {
        if line.is_empty() {
            return;
        }
        if self.history.back().map(|h| h.as_slice()) == Some(line) {
            return;
        }
        if self.history.len() >= HISTORY_CAP {
            self.history.pop_front();
        }
        self.history.push_back(line.to_vec());
    }

    /// Handle a CSI final byte (the letter after `ESC [`).
    fn handle_csi_final(&mut self, final_byte: u8) -> LineEffect {
        match final_byte {
            b'A' => self.history_back(),
            b'B' => self.history_forward(),
            // Left/right cursor movement: silently consume. Mid-line cursor
            // editing is a future polish task; for now visitors typing arrows
            // see no garbage and no movement.
            b'C' | b'D' => LineEffect {
                echo: EchoAction::None,
                line_ready: None,
                raw_byte: None,
                tab_request: None,
            },
            // Unknown CSI: also silently consume.
            _ => LineEffect {
                echo: EchoAction::None,
                line_ready: None,
                raw_byte: None,
                tab_request: None,
            },
        }
    }

    fn history_back(&mut self) -> LineEffect {
        let target = match self.history_pos {
            None => {
                if self.history.is_empty() {
                    return LineEffect {
                        echo: EchoAction::None,
                        line_ready: None,
                        raw_byte: None,
                        tab_request: None,
                    };
                }
                self.saved_partial = Some(self.buffer.clone());
                self.history.len() - 1
            }
            Some(0) => 0, // already at oldest
            Some(p) => p - 1,
        };
        let echo = self.navigate_to_history(target);
        LineEffect {
            echo: if self.mode.echo {
                EchoAction::OwnedBytes(echo)
            } else {
                EchoAction::None
            },
            line_ready: None,
            raw_byte: None,
            tab_request: None,
        }
    }

    fn history_forward(&mut self) -> LineEffect {
        match self.history_pos {
            None => LineEffect {
                echo: EchoAction::None,
                line_ready: None,
                raw_byte: None,
                tab_request: None,
            },
            Some(p) if p + 1 < self.history.len() => {
                let echo = self.navigate_to_history(p + 1);
                LineEffect {
                    echo: if self.mode.echo {
                        EchoAction::OwnedBytes(echo)
                    } else {
                        EchoAction::None
                    },
                    line_ready: None,
                    raw_byte: None,
                    tab_request: None,
                }
            }
            Some(_) => {
                let echo = self.restore_partial();
                LineEffect {
                    echo: if self.mode.echo {
                        EchoAction::OwnedBytes(echo)
                    } else {
                        EchoAction::None
                    },
                    line_ready: None,
                    raw_byte: None,
                    tab_request: None,
                }
            }
        }
    }

    /// Canonical mode: buffer input, emit line on Enter.
    fn handle_byte_canonical(&mut self, byte: u8) -> LineEffect {
        // Drive the CSI parser first. We only act on the FINAL byte of a
        // sequence; the intermediate bytes are silently consumed.
        match self.csi_state {
            CsiState::Esc => {
                if byte == b'[' {
                    self.csi_state = CsiState::Bracket;
                } else {
                    // Bare ESC followed by something we don't recognize. Drop
                    // both — it's safer than treating ESC as text.
                    self.csi_state = CsiState::Idle;
                }
                return LineEffect {
                    echo: EchoAction::None,
                    line_ready: None,
                    raw_byte: None,
                    tab_request: None,
                };
            }
            CsiState::Bracket => {
                self.csi_state = CsiState::Idle;
                return self.handle_csi_final(byte);
            }
            CsiState::Idle => {}
        }

        match byte {
            0x1B => {
                self.csi_state = CsiState::Esc;
                LineEffect {
                    echo: EchoAction::None,
                    line_ready: None,
                    raw_byte: None,
                    tab_request: None,
                }
            }
            0x09 => {
                // TAB in canonical mode: snapshot buffer so the main loop can do
                // VFS-aware completion. We don't echo or modify the buffer here —
                // the main loop will call append_completion + echo if it finds a
                // unique match.
                LineEffect {
                    echo: EchoAction::None,
                    line_ready: None,
                    raw_byte: None,
                    tab_request: Some(self.buffer.clone()),
                }
            }
            0x03 => {
                // Ctrl-C (SIGINT): clear current line buffer and forward an out-of-band
                // marker byte so foreground consumers can interrupt promptly.
                self.buffer.clear();
                self.history_pos = None;
                self.saved_partial = None;
                LineEffect {
                    echo: if self.mode.echo {
                        EchoAction::Bytes(b"^C\n")
                    } else {
                        EchoAction::None
                    },
                    line_ready: Some(alloc::vec![0x03]),
                    raw_byte: None,
                    tab_request: None,
                }
            }
            0x1A => {
                // Ctrl-Z (SIGTSTP): discard current input and forward as an
                // out-of-band marker byte so the TTY can suspend the foreground
                // process group.
                self.buffer.clear();
                self.history_pos = None;
                self.saved_partial = None;
                LineEffect {
                    echo: if self.mode.echo {
                        EchoAction::Bytes(b"^Z\n")
                    } else {
                        EchoAction::None
                    },
                    line_ready: Some(alloc::vec![0x1A]),
                    raw_byte: None,
                    tab_request: None,
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
                        tab_request: None,
                    }
                } else {
                    let line = core::mem::take(&mut self.buffer);
                    self.history_pos = None;
                    self.saved_partial = None;
                    LineEffect {
                        echo: EchoAction::None,
                        line_ready: Some(line),
                        raw_byte: None,
                        tab_request: None,
                    }
                }
            }
            b'\n' => {
                self.buffer.push(byte);
                let line = core::mem::take(&mut self.buffer);
                // Strip trailing newline before pushing to history.
                let cmd_only: &[u8] = if line.ends_with(b"\n") {
                    &line[..line.len() - 1]
                } else {
                    &line
                };
                self.push_history(cmd_only);
                self.history_pos = None;
                self.saved_partial = None;
                LineEffect {
                    echo: if self.mode.echo {
                        EchoAction::Bytes(b"\n")
                    } else {
                        EchoAction::None
                    },
                    line_ready: Some(line),
                    raw_byte: None,
                    tab_request: None,
                }
            }
            0x08 | 0x7f => {
                // Backspace (0x08) or DEL (0x7f). Both delete the last char.
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
                        tab_request: None,
                    }
                } else {
                    LineEffect {
                        echo: EchoAction::None,
                        line_ready: None,
                        raw_byte: None,
                        tab_request: None,
                    }
                }
            }
            _ => {
                self.buffer.push(byte);
                // Any character input invalidates history navigation: the
                // user is now editing the line, not browsing.
                if self.history_pos.is_some() {
                    self.history_pos = None;
                    self.saved_partial = None;
                }
                LineEffect {
                    echo: if self.mode.echo {
                        EchoAction::Byte(byte)
                    } else {
                        EchoAction::None
                    },
                    line_ready: None,
                    raw_byte: None,
                    tab_request: None,
                }
            }
        }
    }

    /// Append completion bytes to the buffer. Called by the TTY main loop after
    /// resolving a tab_request. Does NOT echo; the caller is responsible for
    /// emitting echo bytes to the console.
    pub fn append_completion(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
        // Reset history navigation state — the user is editing fresh again.
        self.history_pos = None;
        self.saved_partial = None;
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
            tab_request: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a sequence of bytes; return the last completed line, if any.
    fn feed(ld: &mut LineDiscipline, bytes: &[u8]) -> Option<Vec<u8>> {
        let mut last_line = None;
        for &b in bytes {
            let eff = ld.handle_byte(b);
            if let Some(line) = eff.line_ready {
                last_line = Some(line);
            }
        }
        last_line
    }

    #[test]
    fn enter_emits_completed_line() {
        let mut ld = LineDiscipline::new();
        let line = feed(&mut ld, b"hello\n");
        assert_eq!(line.as_deref(), Some(b"hello\n".as_ref()));
    }

    #[test]
    fn enter_pushes_command_to_history() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"first\n");
        feed(&mut ld, b"second\n");
        assert_eq!(ld.history.len(), 2);
        assert_eq!(&ld.history[0], b"first");
        assert_eq!(&ld.history[1], b"second");
    }

    #[test]
    fn empty_lines_not_in_history() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"\n");
        feed(&mut ld, b"x\n");
        feed(&mut ld, b"\n");
        assert_eq!(ld.history.len(), 1);
        assert_eq!(&ld.history[0], b"x");
    }

    #[test]
    fn consecutive_duplicate_commands_not_repeated_in_history() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"ls\n");
        feed(&mut ld, b"ls\n");
        feed(&mut ld, b"ls\n");
        assert_eq!(ld.history.len(), 1);
    }

    #[test]
    fn arrow_up_recalls_last_command() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"hello\n");
        // ESC [ A
        feed(&mut ld, b"\x1b[A");
        assert_eq!(ld.buffer, b"hello");
        assert_eq!(ld.history_pos, Some(0));
    }

    #[test]
    fn arrow_up_then_enter_executes_recalled_command() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"hello\n");
        feed(&mut ld, b"\x1b[A");
        let line = feed(&mut ld, b"\n");
        assert_eq!(line.as_deref(), Some(b"hello\n".as_ref()));
    }

    #[test]
    fn arrow_up_through_multiple_history_entries() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"first\n");
        feed(&mut ld, b"second\n");
        feed(&mut ld, b"third\n");
        feed(&mut ld, b"\x1b[A");
        assert_eq!(ld.buffer, b"third");
        feed(&mut ld, b"\x1b[A");
        assert_eq!(ld.buffer, b"second");
        feed(&mut ld, b"\x1b[A");
        assert_eq!(ld.buffer, b"first");
        // At the oldest entry, further ↑ stays put.
        feed(&mut ld, b"\x1b[A");
        assert_eq!(ld.buffer, b"first");
    }

    #[test]
    fn arrow_down_navigates_forward_then_restores_partial() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"first\n");
        feed(&mut ld, b"second\n");
        // User starts typing partial input
        feed(&mut ld, b"par");
        assert_eq!(ld.buffer, b"par");
        // ↑ → recall last
        feed(&mut ld, b"\x1b[A");
        assert_eq!(ld.buffer, b"second");
        // ↑ → older
        feed(&mut ld, b"\x1b[A");
        assert_eq!(ld.buffer, b"first");
        // ↓ → newer
        feed(&mut ld, b"\x1b[B");
        assert_eq!(ld.buffer, b"second");
        // ↓ past newest → restore "par"
        feed(&mut ld, b"\x1b[B");
        assert_eq!(ld.buffer, b"par");
        assert_eq!(ld.history_pos, None);
    }

    #[test]
    fn typing_after_history_recall_invalidates_history_pos() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"hello\n");
        feed(&mut ld, b"\x1b[A");
        assert_eq!(ld.history_pos, Some(0));
        feed(&mut ld, b"x");
        assert_eq!(ld.history_pos, None);
        assert_eq!(ld.buffer, b"hellox");
    }

    #[test]
    fn left_right_arrows_silently_consumed() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"abc");
        feed(&mut ld, b"\x1b[D");
        feed(&mut ld, b"\x1b[C");
        // No mid-line editing yet; buffer unchanged.
        assert_eq!(ld.buffer, b"abc");
    }

    #[test]
    fn backspace_deletes_last_byte() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"abc");
        feed(&mut ld, b"\x08");
        assert_eq!(ld.buffer, b"ab");
    }

    #[test]
    fn del_0x7f_also_acts_as_backspace() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"abc");
        feed(&mut ld, b"\x7f");
        assert_eq!(ld.buffer, b"ab");
    }

    #[test]
    fn ctrl_c_clears_buffer_and_history_pos() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"hello\n");
        feed(&mut ld, b"\x1b[A");
        feed(&mut ld, b"\x03");
        assert_eq!(ld.buffer, b"");
        assert_eq!(ld.history_pos, None);
        assert_eq!(ld.saved_partial, None);
    }

    #[test]
    fn history_capacity_drops_oldest() {
        let mut ld = LineDiscipline::new();
        for i in 0..(HISTORY_CAP + 5) {
            let s = alloc::format!("cmd{}\n", i);
            feed(&mut ld, s.as_bytes());
        }
        assert_eq!(ld.history.len(), HISTORY_CAP);
        // Oldest is "cmd5" (the first 5 dropped).
        assert_eq!(&ld.history[0], b"cmd5");
    }

    #[test]
    fn unknown_csi_silently_consumed() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"abc");
        // ESC [ Z (some random CSI we don't handle)
        feed(&mut ld, b"\x1b[Z");
        assert_eq!(ld.buffer, b"abc");
    }

    #[test]
    fn bare_esc_does_not_corrupt_buffer() {
        let mut ld = LineDiscipline::new();
        feed(&mut ld, b"abc");
        // ESC followed by something that's not '['
        feed(&mut ld, b"\x1bX");
        // Both bytes silently dropped; buffer unchanged.
        assert_eq!(ld.buffer, b"abc");
    }
}
