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
use cluu_proto::pts::Termios;
use crate::syscall::debug_print as dp;

const BACKSPACE_SEQ: &[u8] = b"\x08 \x08";
const HISTORY_CAP: usize = 32;

// ---------------------------------------------------------------------------
// Spec-2 line-discipline output API (unified PTS verb set)
// ---------------------------------------------------------------------------

/// Output of feeding one input byte through POSIX line discipline.
/// Service consumes these and dispatches accordingly.
#[derive(Clone, Debug)]
pub enum LineDiscOutput {
    /// Cooked bytes to deliver to a PTS_READ caller.
    Bytes(alloc::vec::Vec<u8>),
    /// Service should call PROCMGR_PG_SIGNAL(fg_pgid, sig).
    Signal(SignalNum),
    /// Service should write these bytes back as echo.
    Echo(alloc::vec::Vec<u8>),
    /// Canonical EOF reached (VEOF / Ctrl-D). Flush pending line + signal EOF.
    Eof,
    /// Byte consumed; no externally-visible effect (e.g., mid-edit).
    Drop,
}

/// Signal numbers used by the line discipline → service translation.
/// Values match POSIX / newlib `<signal.h>`. Service routes via
/// existing PROCMGR_PG_SIGNAL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalNum {
    SIGINT  = 2,
    SIGQUIT = 3,
    SIGTSTP = 20,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermiosErr {
    InvalidVmin,
    InvalidCcc,
    Unsupported,
}

// ---------------------------------------------------------------------------
// Legacy line-effect API (kept for existing shell UX)
// ---------------------------------------------------------------------------

/// Output of processing a single input byte.
pub struct LineEffect {
    pub echo: EchoAction,
    pub line_ready: Option<Vec<u8>>,
    /// In raw mode, each byte is delivered immediately (no line buffering).
    pub raw_byte: Option<u8>,
    /// When TAB was pressed in canonical mode, this carries a snapshot of the
    /// current line buffer plus the consecutive-TAB count (1 = single tab,
    /// 2+ = double tab requesting list). `None` for any non-TAB byte.
    pub tab_request: Option<(Vec<u8>, u8)>,
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
    // --- Spec-2 termios fields (POSIX line discipline) ---
    pub termios: Termios,
    pending_line: alloc::vec::Vec<u8>,
    output_pending: alloc::vec::Vec<u8>,
    eof_seen: bool,
    last_was_cr: bool,

    // --- Legacy shell-UX fields ---
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
    /// Consecutive TAB count, reset by any non-TAB byte. Drives bash-style
    /// "TAB completes, TAB-TAB lists" behavior.
    consecutive_tabs: u8,
    /// Insertion point within `buffer`. Always in `0..=buffer.len()`. Moved
    /// by ←/→ arrows; advanced/retracted by inserts and backspace.
    cursor: usize,
}

impl LineDiscipline {
    /// Create a new line discipline in canonical+echo mode.
    pub fn new() -> Self {
        Self {
            termios: Termios::default_pts(),
            pending_line: alloc::vec::Vec::new(),
            output_pending: alloc::vec::Vec::new(),
            eof_seen: false,
            last_was_cr: false,
            buffer: Vec::new(),
            mode: TermMode::default(),
            history: VecDeque::new(),
            history_pos: None,
            saved_partial: None,
            csi_state: CsiState::Idle,
            consecutive_tabs: 0,
            cursor: 0,
        }
    }

    /// Update the terminal mode.
    pub fn set_mode(&mut self, mode: TermMode) {
        if !mode.canonical && self.mode.canonical {
            let _ = dp("line_discipline: mode=raw");
        } else if mode.canonical && !self.mode.canonical {
            let _ = dp("line_discipline: mode=canonical");
        }
        self.mode = mode;
        // When switching to raw mode, flush any buffered canonical input
        if !mode.canonical && !self.buffer.is_empty() {
            self.buffer.clear();
            self.cursor = 0;
        }
        self.csi_state = CsiState::Idle;
    }

    // ---- Spec-2 POSIX line-discipline API (unified PTS verb set) ----

    pub fn termios(&self) -> &Termios {
        &self.termios
    }

    pub fn set_termios(&mut self, new: Termios) -> Result<(), TermiosErr> {
        // Basic sanity: accept any termios for now
        let _ = new.c_cc[Termios::VMIN];
        self.termios = new;
        Ok(())
    }

    /// Feed one input byte through POSIX line discipline.
    /// Returns zero or more `LineDiscOutput` events the service must handle.
    pub fn feed_byte(&mut self, byte: u8) -> alloc::vec::Vec<LineDiscOutput> {
        let mut out: alloc::vec::Vec<LineDiscOutput> = alloc::vec::Vec::new();
        let canonical = self.termios.c_lflag & Termios::ICANON != 0;
        let isig      = self.termios.c_lflag & Termios::ISIG   != 0;
        let echo      = self.termios.c_lflag & Termios::ECHO   != 0;
        let echoe     = self.termios.c_lflag & Termios::ECHOE  != 0;
        let echok     = self.termios.c_lflag & Termios::ECHOK  != 0;

        // ISIG translations always come first regardless of canonical mode.
        if isig {
            if byte == self.termios.c_cc[Termios::VINTR] {
                out.push(LineDiscOutput::Signal(SignalNum::SIGINT));
                return out;
            }
            if byte == self.termios.c_cc[Termios::VQUIT] {
                out.push(LineDiscOutput::Signal(SignalNum::SIGQUIT));
                return out;
            }
            if byte == self.termios.c_cc[Termios::VSUSP] {
                out.push(LineDiscOutput::Signal(SignalNum::SIGTSTP));
                return out;
            }
        }

        if !canonical {
            // Raw mode: emit byte immediately.
            out.push(LineDiscOutput::Bytes(alloc::vec![byte]));
            if echo {
                out.push(LineDiscOutput::Echo(alloc::vec![byte]));
            }
            return out;
        }

        // Canonical mode below.
        if byte == self.termios.c_cc[Termios::VEOF] {
            // EOF: flush pending_line, then signal Eof.
            if !self.pending_line.is_empty() {
                out.push(LineDiscOutput::Bytes(core::mem::take(&mut self.pending_line)));
            }
            out.push(LineDiscOutput::Eof);
            return out;
        }
        if byte == self.termios.c_cc[Termios::VERASE] {
            if self.pending_line.pop().is_some() && echoe {
                out.push(LineDiscOutput::Echo(alloc::vec![b'\x08', b' ', b'\x08']));
            }
            return out;
        }
        if byte == self.termios.c_cc[Termios::VKILL] {
            self.pending_line.clear();
            if echok {
                // Visual line clear: CR + clear-to-EOL.
                out.push(LineDiscOutput::Echo(alloc::vec![b'\r', 0x1b, b'[', b'K']));
            }
            return out;
        }
        if byte == self.termios.c_cc[Termios::VWERASE] {
            // Erase last word: pop trailing non-spaces then trailing spaces.
            let mut popped = false;
            while let Some(&b) = self.pending_line.last() {
                if b == b' ' { break; }
                self.pending_line.pop();
                popped = true;
            }
            while let Some(&b) = self.pending_line.last() {
                if b != b' ' { break; }
                self.pending_line.pop();
                popped = true;
            }
            if popped && echoe {
                out.push(LineDiscOutput::Echo(alloc::vec![b'\r', 0x1b, b'[', b'K']));
                out.push(LineDiscOutput::Echo(self.pending_line.clone()));
            }
            return out;
        }
        if byte == b'\n' {
            self.pending_line.push(b'\n');
            let line = core::mem::take(&mut self.pending_line);
            out.push(LineDiscOutput::Bytes(line));
            // Echo newline if ECHO or ECHONL is set.
            if echo || self.termios.c_lflag & Termios::ECHONL != 0 {
                out.push(LineDiscOutput::Echo(alloc::vec![b'\n']));
            }
            return out;
        }
        // ICRNL: translate \r to \n on input.
        if byte == b'\r' && self.termios.c_iflag & Termios::ICRNL != 0 {
            return self.feed_byte(b'\n');
        }
        // INLCR: translate \n to \r on input.
        if byte == b'\n' && self.termios.c_iflag & Termios::INLCR != 0 {
            return self.feed_byte(b'\r');
        }
        // Default: append, echo if requested.
        self.pending_line.push(byte);
        if echo {
            out.push(LineDiscOutput::Echo(alloc::vec![byte]));
        }
        out
    }

    /// Apply OPOST processing to outgoing bytes.
    /// Service calls this before rendering to the framebuffer / VT.
    pub fn process_output(&mut self, bytes: &[u8]) -> alloc::vec::Vec<u8> {
        let opost = self.termios.c_oflag & Termios::OPOST != 0;
        let onlcr = self.termios.c_oflag & Termios::ONLCR != 0;
        if !opost {
            return bytes.to_vec();
        }
        let mut out: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(bytes.len());
        for &b in bytes {
            if b == b'\n' && onlcr {
                out.push(b'\r');
                out.push(b'\n');
            } else {
                out.push(b);
            }
        }
        out
    }

    /// Flush the pending line buffer (used by tcflush(Input)).
    pub fn flush_input(&mut self) {
        self.pending_line.clear();
    }

    // ---- Legacy shell-UX API (kept for existing canonical mode) ----

    /// Process a byte and return echo/line delivery actions.
    pub fn handle_byte(&mut self, byte: u8) -> LineEffect {
        if !self.mode.canonical {
            return self.handle_byte_raw(byte);
        }
        self.handle_byte_canonical(byte)
    }

    /// Build a sequence of bytes that visually erases the current buffer
    /// (BS-space-BS for each char) and writes the new content. Used for
    /// history recall (↑/↓). Handles cursor anywhere in the line by first
    /// echoing the trailing portion to push the visual cursor to end.
    fn redraw_replacement(&self, new_buf: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.buffer.len() * 3 + new_buf.len());
        // Push visual cursor to end of current line so BS-space-BS erases
        // every character regardless of where the logical cursor sat.
        if self.cursor < self.buffer.len() {
            out.extend_from_slice(&self.buffer[self.cursor..]);
        }
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
        self.cursor = new_buf.len();
        self.buffer = new_buf;
        self.history_pos = Some(pos);
        echo
    }

    /// Restore the user's partial input (called when ↓ navigates past the
    /// newest history entry). Emits visual redraw.
    fn restore_partial(&mut self) -> Vec<u8> {
        let new_buf = self.saved_partial.take().unwrap_or_default();
        let echo = self.redraw_replacement(&new_buf);
        self.cursor = new_buf.len();
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
            b'C' => self.cursor_right(),
            b'D' => self.cursor_left(),
            b'H' => self.cursor_home(),
            b'F' => self.cursor_end(),
            // Unknown CSI: also silently consume.
            _ => LineEffect {
                echo: EchoAction::None,
                line_ready: None,
                raw_byte: None,
                tab_request: None,
            },
        }
    }

    fn cursor_left(&mut self) -> LineEffect {
        if self.cursor > 0 && self.mode.echo {
            self.cursor -= 1;
            return LineEffect {
                echo: EchoAction::Byte(0x08),
                line_ready: None,
                raw_byte: None,
                tab_request: None,
            };
        }
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        LineEffect {
            echo: EchoAction::None,
            line_ready: None,
            raw_byte: None,
            tab_request: None,
        }
    }

    fn cursor_right(&mut self) -> LineEffect {
        if self.cursor < self.buffer.len() {
            let ch = self.buffer[self.cursor];
            self.cursor += 1;
            return LineEffect {
                echo: if self.mode.echo { EchoAction::Byte(ch) } else { EchoAction::None },
                line_ready: None,
                raw_byte: None,
                tab_request: None,
            };
        }
        LineEffect {
            echo: EchoAction::None,
            line_ready: None,
            raw_byte: None,
            tab_request: None,
        }
    }

    fn cursor_home(&mut self) -> LineEffect {
        if self.cursor == 0 {
            return LineEffect {
                echo: EchoAction::None,
                line_ready: None,
                raw_byte: None,
                tab_request: None,
            };
        }
        let bs_count = self.cursor;
        self.cursor = 0;
        let echo: Vec<u8> = (0..bs_count).map(|_| 0x08u8).collect();
        LineEffect {
            echo: if self.mode.echo { EchoAction::OwnedBytes(echo) } else { EchoAction::None },
            line_ready: None,
            raw_byte: None,
            tab_request: None,
        }
    }

    fn cursor_end(&mut self) -> LineEffect {
        if self.cursor >= self.buffer.len() {
            return LineEffect {
                echo: EchoAction::None,
                line_ready: None,
                raw_byte: None,
                tab_request: None,
            };
        }
        let trailing = self.buffer[self.cursor..].to_vec();
        self.cursor = self.buffer.len();
        LineEffect {
            echo: if self.mode.echo { EchoAction::OwnedBytes(trailing) } else { EchoAction::None },
            line_ready: None,
            raw_byte: None,
            tab_request: None,
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
        // Reset consecutive-TAB tracker for any non-TAB byte (including CSI
        // intermediates and bare ESC). The TAB branch below increments it.
        if !(byte == 0x09 && self.csi_state == CsiState::Idle) {
            self.consecutive_tabs = 0;
        }

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
                // unique match. Count consecutive TABs to drive single-vs-double
                // tab semantics (complete vs. list).
                self.consecutive_tabs = self.consecutive_tabs.saturating_add(1);
                LineEffect {
                    echo: EchoAction::None,
                    line_ready: None,
                    raw_byte: None,
                    tab_request: Some((self.buffer.clone(), self.consecutive_tabs)),
                }
            }
            0x03 => {
                // Ctrl-C (SIGINT): clear current line buffer and forward an out-of-band
                // marker byte so foreground consumers can interrupt promptly.
                self.buffer.clear();
                self.cursor = 0;
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
                self.cursor = 0;
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
                    self.cursor = 0;
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
                // Move visual cursor to end so the appended '\n' lands after
                // any trailing text the logical cursor was sitting in front of.
                let trailing: Vec<u8> = self.buffer[self.cursor..].to_vec();
                self.buffer.push(byte);
                let line = core::mem::take(&mut self.buffer);
                // Strip trailing newline before pushing to history.
                let cmd_only: &[u8] = if line.ends_with(b"\n") {
                    &line[..line.len() - 1]
                } else {
                    &line
                };
                self.push_history(cmd_only);
                self.cursor = 0;
                self.history_pos = None;
                self.saved_partial = None;
                let echo = if self.mode.echo {
                    let mut v = trailing;
                    v.push(b'\n');
                    EchoAction::OwnedBytes(v)
                } else {
                    EchoAction::None
                };
                LineEffect {
                    echo,
                    line_ready: Some(line),
                    raw_byte: None,
                    tab_request: None,
                }
            }
            0x08 | 0x7f => {
                // Backspace (0x08) or DEL (0x7f): delete the char before the
                // cursor and shift the trailing portion left by one. If the
                // cursor sits at end-of-line this collapses to BS-space-BS.
                if self.cursor == 0 {
                    LineEffect {
                        echo: EchoAction::None,
                        line_ready: None,
                        raw_byte: None,
                        tab_request: None,
                    }
                } else {
                    self.buffer.remove(self.cursor - 1);
                    self.cursor -= 1;
                    let echo = if self.mode.echo {
                        let mut v = Vec::with_capacity(self.buffer.len() - self.cursor + 4);
                        // Step back over the deleted char.
                        v.push(0x08);
                        // Redraw the rest from new cursor onwards.
                        v.extend_from_slice(&self.buffer[self.cursor..]);
                        // Erase the now-stale trailing char and walk cursor back.
                        v.push(b' ');
                        let back = self.buffer.len() - self.cursor + 1;
                        for _ in 0..back {
                            v.push(0x08);
                        }
                        EchoAction::OwnedBytes(v)
                    } else {
                        EchoAction::None
                    };
                    LineEffect {
                        echo,
                        line_ready: None,
                        raw_byte: None,
                        tab_request: None,
                    }
                }
            }
            _ => {
                // Insert the byte at the cursor position. Any character input
                // invalidates history navigation: the user is now editing the
                // line, not browsing.
                self.buffer.insert(self.cursor, byte);
                self.cursor += 1;
                if self.history_pos.is_some() {
                    self.history_pos = None;
                    self.saved_partial = None;
                }
                let echo = if self.mode.echo {
                    if self.cursor == self.buffer.len() {
                        // Append at end: cheap path, just echo the byte.
                        EchoAction::Byte(byte)
                    } else {
                        // Mid-line insert: echo inserted byte + tail, then walk
                        // visual cursor back to the position right after `byte`.
                        let mut v =
                            Vec::with_capacity(self.buffer.len() - self.cursor + 1);
                        v.push(byte);
                        v.extend_from_slice(&self.buffer[self.cursor..]);
                        let back = self.buffer.len() - self.cursor;
                        for _ in 0..back {
                            v.push(0x08);
                        }
                        EchoAction::OwnedBytes(v)
                    }
                } else {
                    EchoAction::None
                };
                LineEffect {
                    echo,
                    line_ready: None,
                    raw_byte: None,
                    tab_request: None,
                }
            }
        }
    }

    /// Append completion bytes to the buffer. Called by the TTY main loop after
    /// resolving a tab_request. Does NOT echo; the caller is responsible for
    /// emitting echo bytes to the console. The buffer cursor follows the
    /// appended bytes.
    pub fn append_completion(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
        self.cursor = self.buffer.len();
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

impl Default for LineDiscipline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod spec2_tests {
    use super::*;

    #[test]
    fn canonical_line_assembly() {
        let mut ld = LineDiscipline::new();
        ld.feed_byte(b'h');
        ld.feed_byte(b'i');
        let out = ld.feed_byte(b'\n');
        let bytes_emitted: alloc::vec::Vec<u8> = out.iter().filter_map(|e| match e {
            LineDiscOutput::Bytes(b) => Some(b.clone()),
            _ => None,
        }).flatten().collect();
        assert_eq!(bytes_emitted, b"hi\n".to_vec());
    }

    #[test]
    fn vintr_signal_under_isig() {
        let mut ld = LineDiscipline::new();
        let out = ld.feed_byte(0x03); // Ctrl-C
        assert!(out.iter().any(|e| matches!(e, LineDiscOutput::Signal(SignalNum::SIGINT))));
    }

    #[test]
    fn no_signal_when_isig_clear() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !Termios::ISIG;
        let out = ld.feed_byte(0x03);
        assert!(!out.iter().any(|e| matches!(e, LineDiscOutput::Signal(_))));
    }

    #[test]
    fn veof_canonical() {
        let mut ld = LineDiscipline::new();
        let out = ld.feed_byte(0x04); // Ctrl-D
        assert!(out.iter().any(|e| matches!(e, LineDiscOutput::Eof)));
    }

    #[test]
    fn verase_with_echoe() {
        let mut ld = LineDiscipline::new();
        ld.feed_byte(b'a');
        let out = ld.feed_byte(0x7f); // DEL
        let echoed: alloc::vec::Vec<u8> = out.iter().filter_map(|e| match e {
            LineDiscOutput::Echo(b) => Some(b.clone()),
            _ => None,
        }).flatten().collect();
        assert_eq!(echoed, b"\x08 \x08".to_vec());
    }

    #[test]
    fn opost_nl_to_crnl() {
        let mut ld = LineDiscipline::new();
        let out = ld.process_output(b"hi\n");
        assert_eq!(out, b"hi\r\n".to_vec());
    }

    #[test]
    fn icrnl_translates_cr_to_nl() {
        let mut ld = LineDiscipline::new();
        ld.feed_byte(b'a');
        let out = ld.feed_byte(b'\r');
        let bytes_emitted: alloc::vec::Vec<u8> = out.iter().filter_map(|e| match e {
            LineDiscOutput::Bytes(b) => Some(b.clone()),
            _ => None,
        }).flatten().collect();
        assert_eq!(bytes_emitted, b"a\n".to_vec());
    }

    #[test]
    fn raw_mode_passthrough() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !Termios::ICANON;
        let out = ld.feed_byte(b'X');
        let bytes: alloc::vec::Vec<u8> = out.iter().filter_map(|e| match e {
            LineDiscOutput::Bytes(b) => Some(b.clone()),
            _ => None,
        }).flatten().collect();
        assert_eq!(bytes, b"X".to_vec());
    }
}
