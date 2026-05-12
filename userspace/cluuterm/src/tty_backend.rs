//! Cluuterm core state machine and recv loop.

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use libcluu::ansi::{Attr, EraseMode, Event, Parser};
use libcluu::ipc::{
    self, COMP_INPUT_FORWARD_LABEL, COMP_WIN_DAMAGE_LABEL, COMP_WIN_DESTROY_LABEL,
    PTS_CLOSED_LABEL, PTS_READ_LABEL, PTS_UNREGISTER_LABEL, PTS_WRITE_LABEL,
};
use libcluu::ipc::COMP_CLOSE_REQUEST_LABEL;
use libcluu::tty_core::{HistoryRow, LineDiscipline, Scrollback};
use libcluu::types::{IpcFlags, Message};
use libcluu::window_shm::WindowShm;
use libcluu::{debug_print, syscall};

/// Scrollback capacity in rows (matches legacy console `SCROLLBACK_LINES`).
const SCROLLBACK_LINES: usize = 200;

/// Maximum bytes VFS can send in a single PTS_WRITE payload.
/// IPC_MESSAGE_MAX in VFS = 1024; actual payload headroom ≈ 1024 - header.
const PTS_WRITE_MAX: usize = 800;

pub struct Cluuterm {
    pub cols: usize,
    pub rows: usize,
    /// Pointer to the SHM header mapped at SHM_VA.
    pub shm: *mut WindowShm,
    pub pts_id: u32,
    pub window_id: u32,
    /// My endpoint (receives FRAME_READY + INPUT_FORWARD from compositor,
    /// PTS_READ/WRITE from VFS, PTS_CLOSED from VFS).
    pub my_ep: usize,
    /// Compositor client endpoint (for DAMAGE + DESTROY messages).
    pub comp_ep: usize,

    // ── Terminal state ──────────────────────────────────────────────────
    pub parser: Parser,
    pub discipline: LineDiscipline,
    pub scrollback: Scrollback,
    /// Cell character grid: `cols * rows` bytes, row-major.
    pub cells: Vec<u8>,
    /// Foreground colour per cell (ARGB u32).
    pub fg_cells: Vec<u32>,
    /// Background colour per cell (ARGB u32).
    pub bg_cells: Vec<u32>,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub current_attr: Attr,
    /// Bytes queued for the next PTS_READ from VFS (shell stdin).
    pub stdin_buf: VecDeque<u8>,
}

// SAFETY: Cluuterm is single-threaded (cluuterm never spawns threads).
unsafe impl Send for Cluuterm {}

impl Cluuterm {
    pub fn new(
        cols: usize,
        rows: usize,
        shm: *mut WindowShm,
        pts_id: u32,
        window_id: u32,
        my_ep: usize,
        comp_ep: usize,
    ) -> Self {
        let total = cols * rows;
        let default_attr = Attr::default_attr();
        Self {
            cols,
            rows,
            shm,
            pts_id,
            window_id,
            my_ep,
            comp_ep,
            parser: Parser::new(),
            discipline: LineDiscipline::new(),
            scrollback: Scrollback::new(SCROLLBACK_LINES),
            cells: alloc::vec![b' '; total],
            fg_cells: alloc::vec![default_attr.fg; total],
            bg_cells: alloc::vec![default_attr.bg; total],
            cursor_x: 0,
            cursor_y: 0,
            current_attr: default_attr,
            stdin_buf: VecDeque::new(),
        }
    }

    // ── PTS_WRITE — shell/app output → cell grid ───────────────────────

    /// Feed `bytes` from the shell through the ANSI parser and apply events
    /// to the cell grid and scrollback.
    pub fn handle_pts_write(&mut self, bytes: &[u8]) {
        let cols = self.cols;
        let rows = self.rows;
        // Swap the parser out to avoid a double-borrow of `self`.
        let mut parser = core::mem::replace(&mut self.parser, Parser::new());
        parser.feed(bytes, |ev| Self::apply_event_ptr(self as *mut Self, cols, rows, ev));
        self.parser = parser;
        // Notify renderer (Task 17 fills the blit body).
        self.render_and_publish();
    }

    /// Apply a single parser event to the cell grid.
    ///
    /// Uses a raw pointer for `self` so the borrow of `parser` (swapped out
    /// in `handle_pts_write`) does not conflict.
    ///
    /// SAFETY: called only from `handle_pts_write` which ensures no aliasing.
    fn apply_event_ptr(term: *mut Self, cols: usize, rows: usize, ev: Event) {
        let s = unsafe { &mut *term };
        match ev {
            Event::Print(b) => {
                let pos = s.cursor_y * cols + s.cursor_x;
                s.cells[pos] = b;
                s.fg_cells[pos] = s.current_attr.fg;
                s.bg_cells[pos] = s.current_attr.bg;
                s.cursor_x += 1;
                if s.cursor_x >= cols {
                    s.cursor_x = 0;
                    s.cursor_y += 1;
                }
                if s.cursor_y >= rows {
                    s.scroll_up();
                }
            }
            Event::Newline => {
                s.cursor_y += 1;
                if s.cursor_y >= rows {
                    s.scroll_up();
                }
            }
            Event::CarriageReturn => s.cursor_x = 0,
            Event::Backspace => {
                if s.cursor_x > 0 {
                    s.cursor_x -= 1;
                }
            }
            Event::MoveCursorUp(n) => {
                s.cursor_y = s.cursor_y.saturating_sub(n as usize);
            }
            Event::MoveCursorDown(n) => {
                s.cursor_y = (s.cursor_y + n as usize).min(rows - 1);
            }
            Event::MoveCursorLeft(n) => {
                s.cursor_x = s.cursor_x.saturating_sub(n as usize);
            }
            Event::MoveCursorRight(n) => {
                s.cursor_x = (s.cursor_x + n as usize).min(cols - 1);
            }
            Event::MoveCursorAbs { row, col } => {
                s.cursor_y = (row.saturating_sub(1) as usize).min(rows - 1);
                s.cursor_x = (col.saturating_sub(1) as usize).min(cols - 1);
            }
            Event::EraseLine(mode) => {
                let row = s.cursor_y;
                let (start, end) = match mode {
                    EraseMode::ToEnd   => (s.cursor_x, cols),
                    EraseMode::ToStart => (0, s.cursor_x + 1),
                    EraseMode::All     => (0, cols),
                };
                for c in start..end {
                    let i = row * cols + c;
                    s.cells[i]    = b' ';
                    s.fg_cells[i] = s.current_attr.fg;
                    s.bg_cells[i] = s.current_attr.bg;
                }
            }
            Event::EraseDisplay(mode) => {
                let total = cols * rows;
                match mode {
                    EraseMode::All => {
                        for i in 0..total {
                            s.cells[i]    = b' ';
                            s.fg_cells[i] = s.current_attr.fg;
                            s.bg_cells[i] = s.current_attr.bg;
                        }
                        s.cursor_x = 0;
                        s.cursor_y = 0;
                    }
                    EraseMode::ToEnd => {
                        // Current row from cursor_x onward.
                        for c in s.cursor_x..cols {
                            let i = s.cursor_y * cols + c;
                            s.cells[i]    = b' ';
                            s.fg_cells[i] = s.current_attr.fg;
                            s.bg_cells[i] = s.current_attr.bg;
                        }
                        // Rows below.
                        for r in (s.cursor_y + 1)..rows {
                            for c in 0..cols {
                                let i = r * cols + c;
                                s.cells[i]    = b' ';
                                s.fg_cells[i] = s.current_attr.fg;
                                s.bg_cells[i] = s.current_attr.bg;
                            }
                        }
                    }
                    EraseMode::ToStart => {
                        for r in 0..s.cursor_y {
                            for c in 0..cols {
                                let i = r * cols + c;
                                s.cells[i]    = b' ';
                                s.fg_cells[i] = s.current_attr.fg;
                                s.bg_cells[i] = s.current_attr.bg;
                            }
                        }
                        for c in 0..=s.cursor_x {
                            let i = s.cursor_y * cols + c;
                            s.cells[i]    = b' ';
                            s.fg_cells[i] = s.current_attr.fg;
                            s.bg_cells[i] = s.current_attr.bg;
                        }
                    }
                }
            }
            Event::SetAttr(a)  => {
                // Emit a harness-observable marker when a coloured SGR is applied.
                // Only log when the foreground colour differs from the reset default
                // to keep noise low during normal operation.
                if a.fg != Attr::default_attr().fg {
                    let fg = a.fg & 0x00FF_FFFF; // strip alpha
                    let mut buf = *b"cluuterm: ansi sgr fg=000000";
                    let hex = b"0123456789ABCDEF";
                    for i in 0..6usize {
                        buf[22 + i] = hex[((fg >> (4 * (5 - i))) & 0xF) as usize];
                    }
                    // SAFETY: buf is valid ASCII.
                    let s_str = unsafe { core::str::from_utf8_unchecked(&buf) };
                    let _ = debug_print(s_str);
                }
                s.current_attr = a;
            }
            Event::ResetAttr   => s.current_attr = Attr::default_attr(),
            Event::Tab => {
                s.cursor_x = ((s.cursor_x / 8) + 1) * 8;
                if s.cursor_x >= cols {
                    s.cursor_x = cols - 1;
                }
            }
            Event::Bell  => {}
            Event::Scroll(_n) => s.scroll_up(),
        }
    }

    fn scroll_up(&mut self) {
        let cols = self.cols;
        let total = cols * self.rows;
        let row = HistoryRow {
            chars: self.cells[0..cols].to_vec(),
            fg:    self.fg_cells[0..cols].to_vec(),
            bg:    self.bg_cells[0..cols].to_vec(),
        };
        self.scrollback.push(row);
        self.cells.copy_within(cols..total, 0);
        self.fg_cells.copy_within(cols..total, 0);
        self.bg_cells.copy_within(cols..total, 0);
        // Blank the newly exposed bottom row.
        for i in (total - cols)..total {
            self.cells[i]    = b' ';
            self.fg_cells[i] = self.current_attr.fg;
            self.bg_cells[i] = self.current_attr.bg;
        }
        if self.cursor_y > 0 {
            self.cursor_y -= 1;
        }
    }

    fn render_and_publish(&mut self) {
        // Blit the terminal cell grid into the compositor SHM.
        crate::render::render(self);

        // Notify the compositor that the full window interior has changed.
        // Protocol: words[0]=win_id, words[1]=x, words[2]=y, words[3]=w, words[4]=h
        // x/y/w/h are in interior cell coordinates (chrome-relative origin 0,0).
        let dmg = libcluu::types::Message::new(
            COMP_WIN_DAMAGE_LABEL,
            [
                self.window_id as usize,
                0,                 // x
                0,                 // y
                self.cols,         // w  (full interior width)
                self.rows,         // h  (full interior height)
                0,
            ],
            5,
        );
        let _ = ipc::send(self.comp_ep, &dmg, libcluu::types::IpcFlags::empty());
    }

    // ── PTS_READ — shell reads stdin ────────────────────────────────────

    /// Drain up to `max` bytes from the stdin buffer.
    pub fn handle_pts_read(&mut self, max: usize) -> Vec<u8> {
        let n = max.min(self.stdin_buf.len());
        self.stdin_buf.drain(..n).collect()
    }

    // ── Shutdown ────────────────────────────────────────────────────────

    fn shutdown(&mut self) {
        let _ = debug_print("cluuterm: shutdown");
        // Unregister PTS slot (idempotent).
        let vfs_ep = libcluu::registry::lookup_service("vfs:main");
        if let Some(ep) = vfs_ep {
            let mut msg = Message::new(
                PTS_UNREGISTER_LABEL,
                [self.pts_id as usize, 0, 0, 0, 0, 0],
                1,
            );
            let _ = ipc::call(ep, &mut msg, IpcFlags::empty());
        }
        // Destroy compositor window.
        let destroy = Message::new(
            COMP_WIN_DESTROY_LABEL,
            [self.window_id as usize, 0, 0, 0, 0, 0],
            1,
        );
        let _ = ipc::send(self.comp_ep, &destroy, IpcFlags::empty());
    }

    // ── Main recv loop ──────────────────────────────────────────────────

    /// Block-receive from `my_ep` and dispatch messages until shutdown.
    pub fn run(&mut self) {
        let mut buf = [0u8; 1024];
        let tokens = [self.my_ep];

        loop {
            let (_, len) = match syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let Some((msg, payload)) = libcluu::ipc::parse_message(&buf[..len]) else {
                continue;
            };

            match msg.tag.label {
                // ── Shell → VFS → cluuterm: write output bytes ──────────
                PTS_WRITE_LABEL => {
                    // payload = the bytes the shell wrote to its stdout/stderr.
                    self.handle_pts_write(payload);
                    // Ack: errno=0, bytes_written=payload.len().
                    let n = payload.len();
                    let reply_token = libcluu::ipc::extract_reply_id(&msg).unwrap_or(0);
                    if reply_token != 0 {
                        let reply = Message::new(
                            PTS_WRITE_LABEL,
                            [0, n, 0, 0, 0, 0],
                            2,
                        );
                        let _ = ipc::reply(reply_token, &reply, IpcFlags::empty());
                    }
                }

                // ── Shell → VFS → cluuterm: read stdin bytes ─────────────
                PTS_READ_LABEL => {
                    let max = msg.words[1].max(1);
                    let data = self.handle_pts_read(max);
                    let reply_token = libcluu::ipc::extract_reply_id(&msg).unwrap_or(0);
                    if reply_token != 0 {
                        // Reply: words[0]=errno, words[1]=len; payload=bytes.
                        let reply = Message::new(
                            PTS_READ_LABEL,
                            [0, data.len(), 0, 0, 0, 0],
                            2,
                        );
                        let _ = libcluu::ipc::reply_with_payload(reply_token, &reply, &data);
                    }
                }

                // ── VFS: all fds on pts closed ────────────────────────────
                PTS_CLOSED_LABEL => {
                    self.shutdown();
                    return;
                }

                // ── Compositor: forwarded keystroke (Task 16) ─────────────
                COMP_INPUT_FORWARD_LABEL => {
                    crate::input::handle(self, &msg, payload);
                }

                // ── Compositor: window close request ──────────────────────
                COMP_CLOSE_REQUEST_LABEL => {
                    self.shutdown();
                    return;
                }

                // Unknown labels are silently dropped.
                _ => {}
            }
        }
    }
}
