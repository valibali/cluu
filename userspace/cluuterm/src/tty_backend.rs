//! Cluuterm core state machine — PTS_* verb dispatch, rendering, recv loop.

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use libcluu::ansi::{Attr, EraseMode, Event, Parser};
use libcluu::ipc::{
    self, COMP_INPUT_FORWARD_LABEL, COMP_WIN_CONFIGURE_LABEL,
    COMP_WIN_DAMAGE_LABEL, COMP_WIN_DESTROY_LABEL,
};
use libcluu::ipc::{COMP_CLOSE_REQUEST_LABEL, PROCMGR_PG_SIGNAL_LABEL};
use libcluu::registry::RegistryEvent;
use libcluu::time::{TIME_SUBSCRIBE_PERIODIC_LABEL, TIME_TICK_LABEL};
use libcluu::tty_core::{HistoryRow, Scrollback};
use libcluu::tty_core::routing::{route_input_byte, ServiceAction};
use libcluu::types::{IpcFlags, Message};
use libcluu::window_shm::WindowShm;
use libcluu::{debug_print, registry, syscall};

use cluu_wire::pts::{
    PTS_READ_LABEL, PTS_WRITE_LABEL, PTS_POLL_LABEL,
    PTS_GET_TERMIOS_LABEL, PTS_SET_TERMIOS_LABEL,
    PTS_GET_WINSIZE_LABEL, PTS_SET_WINSIZE_LABEL,
    PTS_GET_PGRP_LABEL, PTS_SET_PGRP_LABEL,
    PTS_FLUSH_LABEL, PTS_CLOSED_LABEL,
    WriteRequest, WriteReply,
    PollRequest, PollReply, PollEvents,
    GetTermiosReply, SetTermiosRequest, SetTermiosReply,
    GetWinsizeReply, Winsize, SetWinsizeReply,
    GetPgrpReply, SetPgrpRequest, SetPgrpReply,
    FlushRequest, FlushReply, FlushQueue,
    Termios, PtsErr,
};

/// Scrollback capacity in rows (matches legacy console `SCROLLBACK_LINES`).
const SCROLLBACK_LINES: usize = 200;

/// Signal number for SIGWINCH (POSIX).
const SIGWINCH: u32 = 28;
/// Signal number for SIGTTOU (POSIX) — used for TOSTOP check.
const SIGTTOU: u32 = 22;
/// Signal number for SIGTTIN (POSIX) — used for bg-read check.
const SIGTTIN: u32 = 21;

// ── Per-PTS state ────────────────────────────────────────────────────────────

/// PTS state: line discipline + job-control fields. Owned by Cluuterm.pts.
pub struct Pts {
    pub id: u32,
    pub line_discipline: libcluu::tty_core::line_discipline::LineDiscipline,
    pub fg_pgid: Option<i32>,
    pub winsize: Winsize,
    /// Cooked bytes queued for delivery to the next PTS_READ drain hint.
    pub ready_bytes: VecDeque<u8>,
    /// Set when VFS has sent a PTS_READ drain-hint and bytes were not yet
    /// available.  Cleared once bytes are drained and PTS_READ_DELIVER sent.
    pub drain_requested: Option<u32>,
    /// Set when ^D arrives with no parked reader; drained on next PTS_READ.
    pub eof_pending: bool,
    pub closed: bool,
    procmgr_main: usize,
}

impl Pts {
    fn new(id: u32) -> Self {
        Self {
            id,
            line_discipline: libcluu::tty_core::line_discipline::LineDiscipline::new(),
            fg_pgid: None,
            winsize: Winsize { rows: 24, cols: 80, xpixel: 640, ypixel: 480 },
            ready_bytes: VecDeque::new(),
            drain_requested: None,
            eof_pending: false,
            closed: false,
            procmgr_main: 0,
        }
    }

    fn set_procmgr_ep(&mut self, ep: usize) {
        self.procmgr_main = ep;
    }

    /// Drain up to `max` cooked bytes into a Vec.  Returns `None` if empty.
    fn try_take_cooked_bytes(&mut self, max: u32) -> Option<Vec<u8>> {
        let n = (max as usize).min(self.ready_bytes.len());
        if n == 0 {
            return None;
        }
        let out: Vec<u8> = self.ready_bytes.drain(..n).collect();
        Some(out)
    }

    /// Send `PTS_READ_DELIVER_LABEL` (112) to VFS carrying `bytes`.
    ///
    /// `words[0]` is clobbered by `send_msg_with_payload` with payload_len.
    /// `pts_id` lives in `words[1]`. `words[2]` is 1 iff this delivery
    /// signals EOF (caller should grant 0 bytes to the parked reader);
    /// otherwise an empty `bytes` means "no cooked bytes yet, re-park".
    fn send_deliver(&self, vfs_ep: usize, bytes: &[u8]) {
        self.send_deliver_inner(vfs_ep, bytes, 0);
    }

    fn send_deliver_eof(&self, vfs_ep: usize) {
        self.send_deliver_inner(vfs_ep, &[], 1);
    }

    fn send_deliver_inner(&self, vfs_ep: usize, bytes: &[u8], eof: usize) {
        use cluu_wire::pts::PTS_READ_DELIVER_LABEL;
        let msg = Message::new(
            PTS_READ_DELIVER_LABEL,
            [0, self.id as usize, eof, 0, 0, 0],
            3,
        );
        let _ = libcluu::ipc::send_msg_with_payload(vfs_ep, &msg, bytes);
    }

    /// Send a signal to a process group via procmgr (fire-and-forget).
    fn send_pg_signal(&self, pgid: i32, signum: u32) {
        if self.procmgr_main == 0 {
            return;
        }
        let msg = Message::new(
            PROCMGR_PG_SIGNAL_LABEL,
            [pgid as usize, signum as usize, 0, 0, 0, 0],
            2,
        );
        let _ = ipc::send(self.procmgr_main, &msg, IpcFlags::empty());
    }

    // ══════════════════════════════════════════════════════════════════════
    // PTS_* verb handlers
    // ══════════════════════════════════════════════════════════════════════

    /// PTS_READ_LABEL (100) drain-hint from VFS (fire-and-forget, no reply).
    ///
    /// VFS parked the shell's reply_token and sends this to ask cluuterm to
    /// push cooked bytes via `PTS_READ_DELIVER_LABEL`.
    ///
    /// Wire layout (VFS side uses `send_msg_with_payload`):
    ///   `words[0]` = payload_len (0 — clobbered by send_msg_with_payload)
    ///   `words[1]` = pts_id
    ///   `words[2]` = requested bytes
    ///
    /// If cooked bytes are available, drain + send PTS_READ_DELIVER now.
    /// Otherwise set `drain_requested` so `apply_service_actions` fires when
    /// bytes arrive from keyboard input.
    fn handle_pts_read_drain_hint(&mut self, msg: &Message, vfs_ep: usize) {
        let requested = msg.words[2] as u32;
        if requested == 0 {
            return;
        }
        if self.eof_pending {
            self.send_deliver_eof(vfs_ep);
            self.eof_pending = false;
            return;
        }
        if let Some(bytes) = self.try_take_cooked_bytes(requested) {
            self.send_deliver(vfs_ep, &bytes);
        } else {
            // No bytes yet — remember the request; DeliverBytes arm will send.
            self.drain_requested = Some(requested);
        }
    }

    /// PTS_WRITE_LABEL (101)
    ///
    /// Reply is optional: VFS forwards shell stdout via fire-and-forget
    /// send (no reply slot). The cook/render side-effect must run
    /// regardless so the terminal updates.
    fn handle_pts_write(
        &mut self,
        req: &WriteRequest,
        msg: &Message,
        _caller_pid: u32,
        caller_pgid: i32,
    ) -> Option<Vec<u8>> {
        let reply_token = libcluu::ipc::extract_reply_id(msg);

        // TOSTOP check: background process writing to terminal gets SIGTTOU.
        let lflag = self.line_discipline.termios().c_lflag;
        if lflag & Termios::TOSTOP != 0
            && self.fg_pgid.is_some()
            && self.fg_pgid != Some(caller_pgid)
        {
            self.send_pg_signal(caller_pgid, SIGTTOU);
            if let Some(t) = reply_token {
                reply_err(t, PTS_WRITE_LABEL, PtsErr::Eintr);
            }
            return None;
        }

        let cooked = self.line_discipline.process_output(req);
        if let Some(t) = reply_token {
            reply_ok::<WriteReply>(t, PTS_WRITE_LABEL, Ok(req.len() as u32));
        }
        Some(cooked)
    }

    /// PTS_POLL_LABEL (102)
    fn handle_pts_poll(&mut self, req: PollRequest, msg: &Message) {
        let reply_token = match libcluu::ipc::extract_reply_id(msg) {
            Some(t) => t,
            None => return,
        };
        let mut ready = PollEvents::empty();
        if !self.ready_bytes.is_empty() || self.eof_pending { ready |= PollEvents::POLLIN; }
        if !self.closed                  { ready |= PollEvents::POLLOUT; }
        if self.closed                   { ready |= PollEvents::POLLHUP; }
        reply_ok::<PollReply>(reply_token, PTS_POLL_LABEL, PollReply { ready });
    }

    /// PTS_GET_TERMIOS_LABEL (103)
    fn handle_pts_get_termios(&mut self, msg: &Message) {
        let reply_token = match libcluu::ipc::extract_reply_id(msg) {
            Some(t) => t,
            None => return,
        };
        let t = *self.line_discipline.termios();
        reply_ok::<GetTermiosReply>(reply_token, PTS_GET_TERMIOS_LABEL, t);
    }

    /// PTS_SET_TERMIOS_LABEL (104)
    fn handle_pts_set_termios(&mut self, req: SetTermiosRequest, msg: &Message) {
        let reply_token = match libcluu::ipc::extract_reply_id(msg) {
            Some(t) => t,
            None => return,
        };
        match self.line_discipline.set_termios(req.termios) {
            Ok(()) => reply_ok::<SetTermiosReply>(reply_token, PTS_SET_TERMIOS_LABEL, Ok(())),
            Err(_) => reply_ok::<SetTermiosReply>(reply_token, PTS_SET_TERMIOS_LABEL, Err(PtsErr::EinvalTermios)),
        }
    }

    /// PTS_GET_WINSIZE_LABEL (105)
    fn handle_pts_get_winsize(&mut self, msg: &Message) {
        let reply_token = match libcluu::ipc::extract_reply_id(msg) {
            Some(t) => t,
            None => return,
        };
        reply_ok::<GetWinsizeReply>(reply_token, PTS_GET_WINSIZE_LABEL, self.winsize);
    }

    /// PTS_SET_WINSIZE_LABEL (106)
    fn handle_pts_set_winsize(&mut self, req: Winsize, msg: &Message) {
        let reply_token = match libcluu::ipc::extract_reply_id(msg) {
            Some(t) => t,
            None => return,
        };
        self.winsize = req;
        if let Some(pgid) = self.fg_pgid {
            self.send_pg_signal(pgid, SIGWINCH);
        }
        reply_ok::<SetWinsizeReply>(reply_token, PTS_SET_WINSIZE_LABEL, Ok(()));
    }

    /// PTS_GET_PGRP_LABEL (107)
    fn handle_pts_get_pgrp(&mut self, msg: &Message) {
        let reply_token = match libcluu::ipc::extract_reply_id(msg) {
            Some(t) => t,
            None => return,
        };
        reply_ok::<GetPgrpReply>(reply_token, PTS_GET_PGRP_LABEL, self.fg_pgid.unwrap_or(0));
    }

    /// PTS_SET_PGRP_LABEL (108)
    fn handle_pts_set_pgrp(&mut self, req: SetPgrpRequest, msg: &Message) {
        let reply_token = match libcluu::ipc::extract_reply_id(msg) {
            Some(t) => t,
            None => return,
        };
        self.fg_pgid = Some(req);
        reply_ok::<SetPgrpReply>(reply_token, PTS_SET_PGRP_LABEL, Ok(()));
    }

    /// PTS_FLUSH_LABEL (109)
    fn handle_pts_flush(&mut self, req: FlushRequest, msg: &Message) {
        let reply_token = match libcluu::ipc::extract_reply_id(msg) {
            Some(t) => t,
            None => return,
        };
        match req.queue {
            FlushQueue::Input | FlushQueue::Both => {
                self.line_discipline.flush_input();
                self.ready_bytes.clear();
                self.eof_pending = false;
            }
            _ => {}
        }
        match req.queue {
            FlushQueue::Output | FlushQueue::Both => {
                // Output queue: nothing persistent for PTS — handled at TTY side.
            }
            _ => {}
        }
        reply_ok::<FlushReply>(reply_token, PTS_FLUSH_LABEL, Ok(()));
    }

    /// PTS_CLOSED_LABEL (110)
    fn handle_pts_closed(&mut self) {
        self.closed = true;
        // In the async path, parked reads are tracked on the VFS side.
        // Clear any pending drain request on our side.
        self.drain_requested = None;
    }

    // ── Input routing ───────────────────────────────────────────────────────

    /// Feed one input byte through line discipline → service actions.
    /// Returns a list of `ServiceAction` for the caller to dispatch.
    pub fn on_input_byte(&mut self, byte: u8) -> Vec<ServiceAction> {
        route_input_byte(&mut self.line_discipline, byte)
    }
}

// ── Reply helpers (postcard-serialized IPC replies) ──────────────────────────

fn reply_ok<R: serde::Serialize>(reply_token: usize, label: u32, value: R) {
    let bytes = postcard::to_allocvec(&value).expect("postcard ser");
    let mut msg = Message::new(label, [0, 0, 0, 0, 0, 0], 0);
    msg.words[0] = bytes.len();
    msg.words[1] = cluu_wire::ABI_VERSION as usize;
    let _ = libcluu::ipc::reply_with_payload(reply_token, &msg, &bytes);
}

fn reply_err(reply_token: usize, label: u32, err: PtsErr) {
    // Serialize Err(err) directly — postcard doesn't need a type parameter.
    // We encode a Result<(), PtsErr>::Err(err) as the reply payload.
    let value: core::result::Result<(), PtsErr> = core::result::Result::Err(err);
    let bytes = postcard::to_allocvec(&value).expect("postcard ser");
    let mut msg = Message::new(label, [0, 0, 0, 0, 0, 0], 0);
    msg.words[0] = bytes.len();
    msg.words[1] = cluu_wire::ABI_VERSION as usize;
    let _ = libcluu::ipc::reply_with_payload(reply_token, &msg, &bytes);
}

// ── Terminal cell-grid + rendering (preserved from original) ─────────────────

pub struct Cluuterm {
    pub cols: usize,
    pub rows: usize,
    /// Pointer to the SHM header mapped at SHM_VA.
    pub shm: *mut WindowShm,
    pub pts_id: u32,
    pub window_id: u32,
    /// My endpoint (receives FRAME_READY + INPUT_FORWARD from compositor,
    /// PTS_* from VFS, PTS_CLOSED from VFS).
    pub my_ep: usize,
    /// Compositor client endpoint (for DAMAGE + DESTROY messages).
    pub comp_ep: usize,
    /// VFS main endpoint.  Used to send `PTS_READ_DELIVER_LABEL` replies.
    /// Populated at construction from the same endpoint used for PTS
    /// registration.  0 until explicitly set.
    pub vfs_ep: usize,

    // ── PTS state (unified verb set) ────────────────────────────────────────
    pub pts: Pts,

    // ── Terminal state ──────────────────────────────────────────────────
    pub parser: Parser,
    pub discipline: libcluu::tty_core::line_discipline::LineDiscipline,
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

    // ── Cursor-blink state ──────────────────────────────────────────────
    /// Cached timeserver endpoint; 0 = not yet resolved.
    time_ep: usize,
    /// Whether the 500 ms periodic subscription has been armed.
    blink_armed: bool,
    /// Current blink phase: true → cursor visible, false → cursor hidden.
    blink_phase: bool,
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
        vfs_ep: usize,
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
            vfs_ep,
            pts: Pts::new(pts_id),
            parser: Parser::new(),
            discipline: libcluu::tty_core::line_discipline::LineDiscipline::new(),
            scrollback: Scrollback::new(SCROLLBACK_LINES),
            cells: alloc::vec![b' '; total],
            fg_cells: alloc::vec![default_attr.fg; total],
            bg_cells: alloc::vec![default_attr.bg; total],
            cursor_x: 0,
            cursor_y: 0,
            current_attr: default_attr,
            time_ep: 0,
            blink_armed: false,
            blink_phase: true,
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
                // Treat bare LF as CR+LF (ONLCR semantics). Shell writes
                // typically include only `\n` after a line, and there is
                // no kernel tty driver in the pts data path to translate
                // for us, so without this every line cascades right.
                s.cursor_x = 0;
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

    /// Handle a COMP_WIN_CONFIGURE resize event.
    ///
    /// Reallocates the cell grid to `new_cols` × `new_rows`, updates the
    /// pts winsize, emits SIGWINCH to the foreground process group, and
    /// triggers a full redraw.
    pub fn resize_grid(&mut self, new_cols: usize, new_rows: usize) {
        if new_cols == 0 || new_rows == 0 {
            return;
        }

        let total = new_cols * new_rows;
        let default_attr = Attr::default_attr();

        let mut new_cells = alloc::vec![b' '; total];
        let mut new_fg = alloc::vec![default_attr.fg; total];
        let mut new_bg = alloc::vec![default_attr.bg; total];

        // Copy-over existing content that fits in the new grid.
        let copy_cols = new_cols.min(self.cols);
        let copy_rows = new_rows.min(self.rows);
        for r in 0..copy_rows {
            let old_start = r * self.cols;
            let new_start = r * new_cols;
            new_cells[new_start..new_start + copy_cols]
                .copy_from_slice(&self.cells[old_start..old_start + copy_cols]);
            new_fg[new_start..new_start + copy_cols]
                .copy_from_slice(&self.fg_cells[old_start..old_start + copy_cols]);
            new_bg[new_start..new_start + copy_cols]
                .copy_from_slice(&self.bg_cells[old_start..old_start + copy_cols]);
        }

        self.cells = new_cells;
        self.fg_cells = new_fg;
        self.bg_cells = new_bg;
        self.cols = new_cols;
        self.rows = new_rows;

        // Clamp cursor.
        if self.cursor_x >= new_cols {
            self.cursor_x = new_cols.saturating_sub(1);
        }
        if self.cursor_y >= new_rows {
            self.cursor_y = new_rows.saturating_sub(1);
        }

        // Update pts winsize and emit SIGWINCH.
        let new_ws = Winsize {
            rows: new_rows as u16,
            cols: new_cols as u16,
            xpixel: (new_cols * 8) as u16,
            ypixel: (new_rows * 16) as u16,
        };
        if new_ws != self.pts.winsize {
            self.pts.winsize = new_ws;
            if let Some(pgid) = self.pts.fg_pgid {
                self.pts.send_pg_signal(pgid, SIGWINCH);
            }
        }

        // Full redraw with new dimensions.
        self.render_and_publish();
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

    // ── Input dispatch from routing layer ──────────────────────────────

    /// Apply service actions from `route_input_byte` to this terminal.
    pub fn apply_service_actions(&mut self, actions: Vec<ServiceAction>) {
        for action in actions {
            match action {
                ServiceAction::DeliverBytes(bytes) => {
                    self.pts.ready_bytes.extend(bytes.iter().cloned());
                    // If VFS is waiting for a drain, satisfy it now.
                    if let Some(requested) = self.pts.drain_requested.take() {
                        if let Some(cooked) = self.pts.try_take_cooked_bytes(requested) {
                            let vfs_ep = self.vfs_ep;
                            self.pts.send_deliver(vfs_ep, &cooked);
                        } else {
                            // Bytes were consumed by something else; re-arm.
                            self.pts.drain_requested = Some(requested);
                        }
                    }
                }
                ServiceAction::SignalFgPgrp(sig) => {
                    if let Some(pgid) = self.pts.fg_pgid {
                        self.pts.send_pg_signal(pgid, sig as u32);
                    }
                }
                ServiceAction::Echo(bytes) => {
                    // Echo bytes go through process_output (OPOST) then render.
                    let cooked = self.pts.line_discipline.process_output(&bytes);
                    self.handle_pts_write(&cooked);
                }
                ServiceAction::DeliverEof => {
                    if self.pts.drain_requested.take().is_some() {
                        let vfs_ep = self.vfs_ep;
                        self.pts.send_deliver_eof(vfs_ep);
                    } else {
                        self.pts.eof_pending = true;
                    }
                }
            }
        }
    }

    // ── Shutdown ────────────────────────────────────────────────────────

    fn shutdown(&mut self) {
        let _ = debug_print("cluuterm: shutdown");
        // Mark pts closed; wake pending readers.
        self.pts.handle_pts_closed();
        // Destroy compositor window.
        let destroy = Message::new(
            COMP_WIN_DESTROY_LABEL,
            [self.window_id as usize, 0, 0, 0, 0, 0],
            1,
        );
        let _ = ipc::send(self.comp_ep, &destroy, IpcFlags::empty());
    }

    // ── Main recv loop ──────────────────────────────────────────────────

    /// Toggle cursor visibility in the SHM header and notify the compositor.
    ///
    /// Called on every 500 ms TIME_TICK. Does not re-run the full render
    /// path — only the `cursor_visible` flag and the damage notify change.
    fn tick_blink(&mut self) {
        self.blink_phase = !self.blink_phase;
        let visible: u32 = if self.blink_phase { 1 } else { 0 };
        unsafe {
            core::ptr::write_volatile(
                &mut (*self.shm).cursor_visible as *mut u32,
                visible,
            );
        }
        // Kick compositor so it repaints promptly rather than waiting for
        // its own next tick.  The damage rect covers only the cursor cell.
        // self.cursor_x/y are terminal-grid coords; the SHM cell space adds
        // a (+1, +1) chrome offset, so the compositor-visible cell sits at
        // (cursor_x + 1, cursor_y + 1).
        let cx = self.cursor_x + 1;
        let cy = self.cursor_y + 1;
        let dmg = Message::new(
            COMP_WIN_DAMAGE_LABEL,
            [
                self.window_id as usize,
                cx,      // x (compositor coords — chrome-relative)
                cy,      // y
                1,       // w = 1 cell
                1,       // h = 1 cell
                0,
            ],
            5,
        );
        let _ = ipc::send(self.comp_ep, &dmg, IpcFlags::empty());
    }

    /// Arm the 500 ms periodic tick subscription against a freshly-granted
    /// timeserver endpoint.  Silently degrades (no blink) on any failure.
    fn arm_blink_timer(&mut self) {
        if self.blink_armed || self.time_ep == 0 {
            return;
        }
        let notify_ep = self.my_ep;
        let mut sub = Message::new(
            TIME_SUBSCRIBE_PERIODIC_LABEL,
            [500, notify_ep, 0, 0, 0, 0],
            3,
        );
        if libcluu::ipc::call(self.time_ep, &mut sub, IpcFlags::empty()).is_ok()
            && sub.words[0] == 0
        {
            self.blink_armed = true;
            let _ = debug_print("cluuterm: subscribed to timeserver 500ms blink");
        } else {
            let _ = debug_print("cluuterm: timeserver subscribe failed — no cursor blink");
        }
    }

    /// Block-receive from `my_ep` and dispatch messages until shutdown.
    pub fn run(&mut self) {
        let mut buf = [0u8; 1024];

        // Request a timeserver grant up-front so the tick arrives once
        // timeserver registers.  Failure is non-fatal: blink is cosmetic.
        if registry::request_subscription("timeserver", "main").is_ok() {
            let _ = debug_print("cluuterm: timeserver subscription requested");
        } else {
            let _ = debug_print("cluuterm: timeserver subscription request failed — no blink");
        }

        // Request procmgr:main subscription for pg_signal (job control).
        if registry::request_subscription("procmgr", "main").is_ok() {
            let _ = debug_print("cluuterm: procmgr main subscription requested");
        }

        // The registry control endpoint carries Grant / SubscribeStatus events.
        // Include it alongside my_ep so we receive the timeserver grant.
        let ctrl_ep = registry::control_endpoint();
        // Index of the registry endpoint in the `tokens` array below.
        const REGISTRY_IDX: usize = 1;

        loop {
            // Rebuild the tokens slice each iteration so a zero ctrl_ep is
            // simply excluded (the kernel rejects 0-valued endpoint tokens).
            let (_, len) = if ctrl_ep != 0 {
                let tokens = [self.my_ep, ctrl_ep];
                match syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            } else {
                let tokens = [self.my_ep];
                match syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            };

            let Some((msg, payload)) = libcluu::ipc::parse_message(&buf[..len]) else {
                continue;
            };

            // ── Registry control: Grant / SubscribeStatus ──────────────
            // We do a label-based check first because the registry control
            // endpoint is only in the tokens array when ctrl_ep != 0, but
            // the labels are unambiguous across all senders.
            if let Ok(Some(event)) = registry::handle_incoming_message(&msg, payload) {
                match event {
                    RegistryEvent::Grant { service_name, name, token } => {
                        if service_name == "timeserver" && name == "main" {
                            self.time_ep = token;
                            self.arm_blink_timer();
                        }
                        if service_name == "procmgr" && name == "main" {
                            self.pts.set_procmgr_ep(token);
                        }
                    }
                    RegistryEvent::SubscribeStatus { code } => {
                        if code != 0 {
                            // Registry refused or failed — service stays offline.
                            let _ = debug_print("cluuterm: subscription status non-zero");
                        }
                    }
                }
                continue;
            }

            match msg.tag.label {
                // ── Timeserver: 500 ms periodic tick ─────────────────────
                TIME_TICK_LABEL => {
                    self.tick_blink();
                }

                // ═══════════════════════════════════════════════════════
                // PTS_* unified verb set (labels 100-110)
                // ═══════════════════════════════════════════════════════

                // ── PTS_WRITE (101): Shell → VFS → cluuterm output ─────
                PTS_WRITE_LABEL => {
                    // VFS sends raw bytes via send_msg_with_payload.
                    let req: WriteRequest = payload.to_vec();
                    if let Some(cooked) = self.pts.handle_pts_write(&req, &msg, 0, 0) {
                        self.handle_pts_write(&cooked);
                    }
                }

                // ── PTS_READ (100): drain-hint from VFS ────────────────
                //
                // VFS sends this fire-and-forget after parking the shell's
                // reply_token.  `words[1]` = pts_id, `words[2]` = requested.
                // We drain cooked bytes and push PTS_READ_DELIVER back to VFS.
                PTS_READ_LABEL => {
                    let vfs_ep = self.vfs_ep;
                    self.pts.handle_pts_read_drain_hint(&msg, vfs_ep);
                }

                // ── PTS_SET_PGRP (138): shell tcsetpgrp via VFS proxy ──
                PTS_SET_PGRP_LABEL => {
                    let pgid: SetPgrpRequest = match postcard::from_bytes(payload) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    self.pts.handle_pts_set_pgrp(pgid, &msg);
                }

                // ── PTS_GET_TERMIOS (133): tcgetattr via VFS proxy ─────
                PTS_GET_TERMIOS_LABEL => {
                    self.pts.handle_pts_get_termios(&msg);
                }

                // ── PTS_SET_TERMIOS (134): tcsetattr via VFS proxy ─────
                PTS_SET_TERMIOS_LABEL => {
                    let req: SetTermiosRequest = match postcard::from_bytes(payload) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    self.pts.handle_pts_set_termios(req, &msg);
                }

                // ── PTS_CLOSED (110): VFS notifies that all fds closed ──
                PTS_CLOSED_LABEL => {
                    self.shutdown();
                    return;
                }

                // ── Compositor: forwarded keystroke (Task 16) ───────────
                COMP_INPUT_FORWARD_LABEL => {
                    if msg.words[5] == 99 {
                        self.shutdown();
                        return;
                    }
                    crate::input::handle(self, &msg, payload);
                }

                // ── Compositor: window configure (resize) ───────────────
                COMP_WIN_CONFIGURE_LABEL => {
                    let interior_w = msg.words[1] as usize;
                    let interior_h = msg.words[2] as usize;
                    if interior_w > 0 && interior_h > 0
                        && (interior_w != self.cols || interior_h != self.rows)
                    {
                        self.resize_grid(interior_w, interior_h);
                    }
                }

                // ── Compositor: window close request ────────────────────
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