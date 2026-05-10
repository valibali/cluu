//! Framebuffer console renderer.
//!
//! Each VT is represented by a self-contained `VtScreen` that owns its cell
//! grid, cursor state, ANSI parser, and colors. Writes to any VT update only
//! that VtScreen's in-memory cell grid — no framebuffer access needed.
//!
//! The `Console` struct manages the framebuffer backend and renders the active
//! VtScreen to the display. This eliminates the context-switch trick where
//! inactive VT writes temporarily swapped state into "active registers".

extern crate alloc;

use alloc::vec::Vec;

use crate::backend::ConsoleBackend;
use libcluu::ipc::{
    extract_reply_id, reply, CONSOLE_BLINK_LABEL, CONSOLE_CLEAR_LABEL, CONSOLE_CURSOR_LABEL,
    CONSOLE_FB_INFO_LABEL, CONSOLE_WRITE_LABEL, CONSOLE_WRITE_SYNC_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::Result;

const GLYPH_W: usize = 8;
const GLYPH_H: usize = 16;

const COLOR_BG: u32 = 0x00000000;
const COLOR_FG: u32 = 0x00FFFFFF;
const ANSI_COLORS: [u32; 8] = [
    0x00000000, // black
    0x00AA0000, // red
    0x0000AA00, // green
    0x00AA5500, // yellow/brown
    0x000000AA, // blue
    0x00AA00AA, // magenta
    0x0000AAAA, // cyan
    0x00AAAAAA, // white/gray
];
const ANSI_BRIGHT_COLORS: [u32; 8] = [
    0x00555555, // bright black
    0x00FF5555, // bright red
    0x0055FF55, // bright green
    0x00FFFF55, // bright yellow
    0x005555FF, // bright blue
    0x00FF55FF, // bright magenta
    0x0055FFFF, // bright cyan
    0x00FFFFFF, // bright white
];

/// Number of virtual terminals supported.
const VT_COUNT: usize = 4;

const SCROLLBACK_LINES: usize = 200;
const SCROLL_PAGE_LINES: usize = 10;

/// A single row saved in the scrollback history buffer.
struct HistoryRow {
    chars: Vec<u8>,
    fg: Vec<u32>,
    bg: Vec<u32>,
}

/// ANSI escape sequence parser state.
#[derive(Clone, Copy, PartialEq)]
enum EscState {
    /// Normal character processing.
    Normal,
    /// Seen ESC (0x1B), waiting for '[' or other sequence introducer.
    Escape,
    /// Inside a CSI sequence (ESC [ ...), accumulating parameters.
    Csi,
}

/// Self-contained virtual terminal screen.
///
/// Each VtScreen owns its cell grid, cursor state, ANSI parser, and colors.
/// It can process writes independently of other VTs and without touching the
/// framebuffer — it only updates the in-memory cell grid.
struct VtScreen {
    cols: usize,
    rows: usize,
    cursor_x: usize,
    cursor_y: usize,
    cursor_visible: bool,
    blink_enabled: bool,
    cells: Vec<u8>,
    fg_cells: Vec<u32>,
    bg_cells: Vec<u32>,
    current_fg: u32,
    current_bg: u32,
    dirty_cells: Vec<(usize, usize)>,
    /// Set when a full repaint is needed (e.g., after scroll or clear).
    needs_repaint: bool,
    esc_state: EscState,
    esc_params: [u16; 4],
    esc_param_count: usize,
    esc_current_param: u16,
    /// Ring buffer of scrolled-off rows.
    history: Vec<HistoryRow>,
    history_start: usize,
    history_len: usize,
    /// 0 = live view, >0 = scrolled back N lines into history.
    viewport_offset: usize,
}

impl VtScreen {
    fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cursor_x: 0,
            cursor_y: 0,
            cursor_visible: true,
            blink_enabled: true,
            cells: alloc::vec![b' '; cols * rows],
            fg_cells: alloc::vec![COLOR_FG; cols * rows],
            bg_cells: alloc::vec![COLOR_BG; cols * rows],
            current_fg: COLOR_FG,
            current_bg: COLOR_BG,
            dirty_cells: Vec::new(),
            needs_repaint: false,
            esc_state: EscState::Normal,
            esc_params: [0; 4],
            esc_param_count: 0,
            esc_current_param: 0,
            history: Vec::new(),
            history_start: 0,
            history_len: 0,
            viewport_offset: 0,
        }
    }

    /// Process a byte stream, updating cells, cursor, and parser state.
    ///
    /// This only modifies the in-memory cell grid — no framebuffer writes.
    /// The caller (Console) is responsible for rendering after this returns.
    fn write_bytes(&mut self, bytes: &[u8]) {
        self.dirty_cells.clear();

        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];

            if b < 0x80 {
                self.put_char(b);
                i += 1;
                continue;
            }

            let (codepoint, seq_len) = if b & 0xE0 == 0xC0 && i + 1 < bytes.len() {
                let cp = ((b as u32 & 0x1F) << 6) | (bytes[i + 1] as u32 & 0x3F);
                (cp, 2)
            } else if b & 0xF0 == 0xE0 && i + 2 < bytes.len() {
                let cp = ((b as u32 & 0x0F) << 12)
                    | ((bytes[i + 1] as u32 & 0x3F) << 6)
                    | (bytes[i + 2] as u32 & 0x3F);
                (cp, 3)
            } else if b & 0xF8 == 0xF0 && i + 3 < bytes.len() {
                let cp = ((b as u32 & 0x07) << 18)
                    | ((bytes[i + 1] as u32 & 0x3F) << 12)
                    | ((bytes[i + 2] as u32 & 0x3F) << 6)
                    | (bytes[i + 3] as u32 & 0x3F);
                (cp, 4)
            } else {
                self.put_char(b'?');
                i += 1;
                continue;
            };

            self.put_char(unicode_to_cp437(codepoint));
            i += seq_len;
        }
    }

    /// Reset all cells and cursor to defaults.
    fn clear(&mut self) {
        self.cells.fill(b' ');
        self.fg_cells.fill(COLOR_FG);
        self.bg_cells.fill(COLOR_BG);
        self.current_fg = COLOR_FG;
        self.current_bg = COLOR_BG;
        self.dirty_cells.clear();
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.needs_repaint = true;
    }

    fn put_char(&mut self, ch: u8) {
        match self.esc_state {
            EscState::Normal => match ch {
                0x1B => {
                    self.esc_state = EscState::Escape;
                }
                b'\n' => self.newline(),
                b'\r' => {
                    self.cursor_x = 0;
                }
                0x08 => {
                    if self.cursor_x > 0 {
                        self.cursor_x -= 1;
                    } else if self.cursor_y > 0 {
                        self.cursor_y -= 1;
                        self.cursor_x = self.cols.saturating_sub(1);
                    }
                }
                _ => {
                    self.set_cell(
                        self.cursor_x,
                        self.cursor_y,
                        ch,
                        self.current_fg,
                        self.current_bg,
                    );
                    self.cursor_x += 1;
                    if self.cursor_x >= self.cols {
                        self.newline();
                    }
                }
            },
            EscState::Escape => match ch {
                b'[' => {
                    self.esc_state = EscState::Csi;
                    self.esc_params = [0; 4];
                    self.esc_param_count = 0;
                    self.esc_current_param = 0;
                }
                _ => {
                    self.esc_state = EscState::Normal;
                }
            },
            EscState::Csi => {
                if ch.is_ascii_digit() {
                    self.esc_current_param = self
                        .esc_current_param
                        .saturating_mul(10)
                        .saturating_add((ch - b'0') as u16);
                } else if ch == b';' {
                    if self.esc_param_count < self.esc_params.len() {
                        self.esc_params[self.esc_param_count] = self.esc_current_param;
                        self.esc_param_count += 1;
                    }
                    self.esc_current_param = 0;
                } else if ch == b'~' {
                    if self.esc_param_count < self.esc_params.len() {
                        self.esc_params[self.esc_param_count] = self.esc_current_param;
                        self.esc_param_count += 1;
                    }
                    self.esc_state = EscState::Normal;
                } else if (0x40..=0x7E).contains(&ch) {
                    if self.esc_param_count < self.esc_params.len() {
                        self.esc_params[self.esc_param_count] = self.esc_current_param;
                        self.esc_param_count += 1;
                    }
                    self.dispatch_csi(ch);
                    self.esc_state = EscState::Normal;
                } else {
                    self.esc_state = EscState::Normal;
                }
            }
        }
    }

    fn dispatch_csi(&mut self, cmd: u8) {
        let p0 = if self.esc_param_count > 0 {
            self.esc_params[0]
        } else {
            0
        };
        let p1 = if self.esc_param_count > 1 {
            self.esc_params[1]
        } else {
            0
        };

        match cmd {
            b'A' => {
                let n = if p0 == 0 { 1 } else { p0 as usize };
                self.cursor_y = self.cursor_y.saturating_sub(n);
            }
            b'B' => {
                let n = if p0 == 0 { 1 } else { p0 as usize };
                self.cursor_y = (self.cursor_y + n).min(self.rows.saturating_sub(1));
            }
            b'C' => {
                let n = if p0 == 0 { 1 } else { p0 as usize };
                self.cursor_x = (self.cursor_x + n).min(self.cols.saturating_sub(1));
            }
            b'D' => {
                let n = if p0 == 0 { 1 } else { p0 as usize };
                self.cursor_x = self.cursor_x.saturating_sub(n);
            }
            b'H' => {
                let row = if p0 == 0 { 1 } else { p0 as usize };
                let col = if p1 == 0 { 1 } else { p1 as usize };
                self.cursor_y = (row - 1).min(self.rows.saturating_sub(1));
                self.cursor_x = (col - 1).min(self.cols.saturating_sub(1));
            }
            b'J' => {
                match p0 {
                    2 => {
                        self.clear();
                    }
                    0 => {
                        for x in self.cursor_x..self.cols {
                            self.set_cell(x, self.cursor_y, b' ', self.current_fg, self.current_bg);
                        }
                        for y in (self.cursor_y + 1)..self.rows {
                            for x in 0..self.cols {
                                self.set_cell(x, y, b' ', self.current_fg, self.current_bg);
                            }
                        }
                    }
                    _ => {}
                }
            }
            b'K' => {
                match p0 {
                    0 => {
                        for x in self.cursor_x..self.cols {
                            self.set_cell(x, self.cursor_y, b' ', self.current_fg, self.current_bg);
                        }
                    }
                    1 => {
                        for x in 0..=self.cursor_x.min(self.cols.saturating_sub(1)) {
                            self.set_cell(x, self.cursor_y, b' ', self.current_fg, self.current_bg);
                        }
                    }
                    2 => {
                        for x in 0..self.cols {
                            self.set_cell(x, self.cursor_y, b' ', self.current_fg, self.current_bg);
                        }
                    }
                    _ => {}
                }
            }
            b'm' => {
                if self.esc_param_count == 0 {
                    self.current_fg = COLOR_FG;
                    self.current_bg = COLOR_BG;
                    return;
                }

                for idx in 0..self.esc_param_count {
                    let p = self.esc_params[idx];
                    match p {
                        0 => {
                            self.current_fg = COLOR_FG;
                            self.current_bg = COLOR_BG;
                        }
                        30..=37 => self.current_fg = ANSI_COLORS[(p - 30) as usize],
                        40..=47 => self.current_bg = ANSI_COLORS[(p - 40) as usize],
                        90..=97 => self.current_fg = ANSI_BRIGHT_COLORS[(p - 90) as usize],
                        100..=107 => self.current_bg = ANSI_BRIGHT_COLORS[(p - 100) as usize],
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn newline(&mut self) {
        self.cursor_x = 0;
        if self.cursor_y + 1 >= self.rows {
            self.scroll_up();
        } else {
            self.cursor_y += 1;
        }
    }

    /// Scroll the grid up by one row (cell data only, no framebuffer).
    fn scroll_up(&mut self) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }
        // Save the top row to scrollback history before shifting.
        self.push_history_row();

        let w = self.cols;

        // Shift rows 1..end up by one row.
        self.cells.copy_within(w.., 0);
        self.fg_cells.copy_within(w.., 0);
        self.bg_cells.copy_within(w.., 0);

        // Clear last row.
        let last = (self.rows - 1) * w;
        self.cells[last..last + w].fill(b' ');
        self.fg_cells[last..last + w].fill(self.current_fg);
        self.bg_cells[last..last + w].fill(self.current_bg);

        // Previous dirty positions are invalid after the shift.
        self.dirty_cells.clear();
        self.needs_repaint = true;
        // New output returns to live view.
        self.viewport_offset = 0;

        self.cursor_y = self.rows - 1;
    }

    /// Save the top row (row 0) to the scrollback history ring buffer.
    fn push_history_row(&mut self) {
        let w = self.cols;
        let row = HistoryRow {
            chars: self.cells[..w].to_vec(),
            fg: self.fg_cells[..w].to_vec(),
            bg: self.bg_cells[..w].to_vec(),
        };
        if self.history.len() < SCROLLBACK_LINES {
            self.history.push(row);
            self.history_len = self.history.len();
        } else {
            let idx = (self.history_start + self.history_len) % SCROLLBACK_LINES;
            self.history[idx] = row;
            self.history_start = (self.history_start + 1) % SCROLLBACK_LINES;
            // history_len stays at SCROLLBACK_LINES
        }
    }

    /// Adjust the viewport offset for scrollback navigation.
    /// Positive delta = scroll back (show older), negative = scroll forward (show newer).
    fn scroll_viewport(&mut self, delta: isize) {
        let new_offset = (self.viewport_offset as isize + delta)
            .max(0)
            .min(self.history_len as isize) as usize;
        if new_offset != self.viewport_offset {
            self.viewport_offset = new_offset;
            self.needs_repaint = true;
        }
    }

    /// Update one grid cell and mark it as dirty.
    fn set_cell(&mut self, x: usize, y: usize, ch: u8, fg: u32, bg: u32) {
        if x >= self.cols || y >= self.rows {
            return;
        }
        let idx = y * self.cols + x;
        if self.cells[idx] != ch || self.fg_cells[idx] != fg || self.bg_cells[idx] != bg {
            self.cells[idx] = ch;
            self.fg_cells[idx] = fg;
            self.bg_cells[idx] = bg;
            let pos = (x, y);
            if !self.dirty_cells.contains(&pos) {
                self.dirty_cells.push(pos);
            }
        }
    }
}

/// Console renderer backed by a pluggable pixel backend.
///
/// Each VT is a self-contained VtScreen. Writes to any VT update only
/// that VtScreen's cell grid. The renderer reads from vt_screens[active_vt]
/// and renders to the framebuffer backend.
pub struct Console<B: ConsoleBackend> {
    backend: B,
    width: usize,
    height: usize,
    /// Framebuffer physical address (for serving to apps).
    fb_phys: u64,
    /// Framebuffer size in bytes.
    fb_size: u64,
    cols: usize,
    rows: usize,
    /// Whether this console is the active (visible) VT.
    active: bool,
    /// Which VT is currently displayed.
    active_vt: usize,
    /// Per-VT screen state. All VTs are pre-created.
    vt_screens: [VtScreen; VT_COUNT],
    // ── Rendering state (framebuffer cursor tracking) ──
    last_cursor_x: usize,
    last_cursor_y: usize,
}

impl<B: ConsoleBackend> Console<B> {
    /// Create a new console renderer and clear its contents.
    pub fn new(backend: B, fb_phys: u64, fb_size: u64) -> Self {
        let width = backend.width();
        let height = backend.height();
        let cols = width / GLYPH_W;
        let rows = height / GLYPH_H;
        let mut console = Self {
            backend,
            width,
            height,
            fb_phys,
            fb_size,
            cols,
            rows,
            active: true,
            active_vt: 0,
            vt_screens: [
                VtScreen::new(cols, rows),
                VtScreen::new(cols, rows),
                VtScreen::new(cols, rows),
                VtScreen::new(cols, rows),
            ],
            last_cursor_x: 0,
            last_cursor_y: 0,
        };
        console.repaint_all();
        console
    }

    /// Set the initial active state (called once at startup from boot params).
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    // ── Multi-VT methods ──────────────────────────────────────────────

    /// Create a new VT buffer at the given index (no-op: all VTs pre-created).
    pub fn create_vt(&mut self, _vt_index: usize) {}

    /// Scroll a VT's viewport for scrollback navigation.
    pub fn scroll_vt(&mut self, vt_index: usize, direction: usize) {
        if vt_index >= VT_COUNT { return; }
        let delta = if direction == 0 {
            SCROLL_PAGE_LINES as isize   // scroll back (up = show older)
        } else {
            -(SCROLL_PAGE_LINES as isize) // scroll forward (down = show newer)
        };
        self.vt_screens[vt_index].scroll_viewport(delta);
        if vt_index == self.active_vt {
            self.render_active_vt();
            self.backend.flush();
        }
    }

    /// Switch the active VT display.
    pub fn switch_vt(&mut self, new_vt: usize) {
        if new_vt >= VT_COUNT || new_vt == self.active_vt {
            return;
        }
        self.active_vt = new_vt;
        self.active = true;
        self.repaint_all();
        self.vt_screens[new_vt].needs_repaint = false;
        self.vt_screens[new_vt].dirty_cells.clear();
        self.backend.flush();
    }

    /// Deactivate a specific VT (called by vtmgr before switching).
    pub fn deactivate_vt(&mut self, _vt_index: usize) {
        self.active = false;
    }

    /// Write data to a specific VT index.
    ///
    /// The VtScreen processes the bytes independently. If this is the active
    /// VT, the changes are rendered to the framebuffer.
    pub fn write_to_vt(&mut self, vt_index: usize, payload: &[u8]) {
        if vt_index >= VT_COUNT {
            return;
        }
        self.vt_screens[vt_index].write_bytes(payload);
        if vt_index == self.active_vt {
            self.render_active_vt();
        }
    }

    /// Handle a console IPC message and apply the requested action.
    pub fn handle_message(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        match msg.tag.label {
            CONSOLE_WRITE_LABEL => {
                let vt = self.active_vt;
                self.vt_screens[vt].write_bytes(payload);
                if self.active {
                    self.render_active_vt();
                }
            }
            CONSOLE_WRITE_SYNC_LABEL => {
                let vt = self.active_vt;
                self.vt_screens[vt].write_bytes(payload);
                if self.active {
                    self.render_active_vt();
                }
                if let Some(reply_token) = extract_reply_id(msg) {
                    let reply_msg = Message::new(CONSOLE_WRITE_SYNC_LABEL, [0; 6], 0);
                    let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            }
            CONSOLE_CLEAR_LABEL => {
                let vt = self.active_vt;
                self.vt_screens[vt].clear();
                if self.active {
                    self.render_active_vt();
                }
            }
            CONSOLE_CURSOR_LABEL => {
                let vt = self.active_vt;
                let cols = self.cols;
                let rows = self.rows;
                self.vt_screens[vt].cursor_x = msg.words[0].min(cols.saturating_sub(1));
                self.vt_screens[vt].cursor_y = msg.words[1].min(rows.saturating_sub(1));
                if self.active {
                    self.redraw_cursor();
                }
            }
            CONSOLE_BLINK_LABEL => {
                let vt = self.active_vt;
                self.vt_screens[vt].blink_enabled = msg.words[0] != 0;
                if !self.vt_screens[vt].blink_enabled {
                    self.vt_screens[vt].cursor_visible = true;
                }
                if self.active {
                    self.redraw_cursor();
                }
            }
            CONSOLE_FB_INFO_LABEL => {
                if let Some(reply_token) = extract_reply_id(msg) {
                    let reply_msg = Message::new(
                        CONSOLE_FB_INFO_LABEL,
                        [
                            self.fb_phys as usize,
                            self.fb_size as usize,
                            self.width,
                            self.height,
                            self.backend.pitch(),
                            4, // bytes per pixel (BGRA32)
                        ],
                        6,
                    );
                    let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Advance the blink timer; called on IPC timeout.
    /// Returns true if cursor visibility changed.
    pub fn tick(&mut self) -> bool {
        if !self.active {
            return false;
        }
        let vt = self.active_vt;
        if !self.vt_screens[vt].blink_enabled {
            return false;
        }
        self.vt_screens[vt].cursor_visible = !self.vt_screens[vt].cursor_visible;
        self.redraw_cursor();
        true
    }

    /// Flush any buffered writes to the display.
    ///
    /// For double-buffered backends, this copies the dirty region to the frontbuffer.
    /// Should be called after rendering operations and periodically for cursor blink.
    /// No-op when this console is inactive (another VT is visible).
    #[inline]
    pub fn flush(&mut self) {
        if !self.active {
            return;
        }
        self.backend.flush();
    }

    /// Check if the backend has pending changes to flush.
    #[inline]
    #[allow(dead_code)]
    pub fn is_dirty(&self) -> bool {
        self.backend.is_dirty()
    }

    // ── Private rendering methods ──────────────────────────────────────

    /// Render pending changes from the active VtScreen to the framebuffer.
    fn render_active_vt(&mut self) {
        if !self.active {
            return;
        }
        let vt = self.active_vt;
        if self.vt_screens[vt].needs_repaint || self.vt_screens[vt].viewport_offset > 0 {
            self.repaint_all();
            self.vt_screens[vt].needs_repaint = false;
            self.vt_screens[vt].dirty_cells.clear();
        } else {
            self.flush_dirty_cells();
            self.redraw_cursor();
        }
    }

    /// Repaint the entire grid from the active VtScreen's cell data.
    ///
    /// When `viewport_offset > 0`, the top rows show scrollback history and
    /// the remaining rows show the current screen shifted down.
    fn repaint_all(&mut self) {
        let vt_idx = self.active_vt;
        let cols = self.cols;
        let rows = self.rows;

        self.backend
            .fill_rect(0, 0, self.width, self.height, COLOR_BG);

        let offset = self.vt_screens[vt_idx].viewport_offset;
        let hist_len = self.vt_screens[vt_idx].history_len;
        let hist_start = self.vt_screens[vt_idx].history_start;

        for y in 0..rows {
            // With offset=N: first N screen rows come from history, rest from current.
            // Screen row y maps to:
            //   if y < offset: history row (hist_len - offset + y)
            //   else: current screen row (y - offset)
            if y < offset && offset <= hist_len {
                // Render from history
                let h_idx = hist_len - offset + y;
                let ring_idx = (hist_start + h_idx) % self.vt_screens[vt_idx].history.len().max(1);
                if ring_idx < self.vt_screens[vt_idx].history.len() {
                    let row = &self.vt_screens[vt_idx].history[ring_idx];
                    for x in 0..cols.min(row.chars.len()) {
                        let ch = row.chars[x];
                        let fg = row.fg[x];
                        let bg = row.bg[x];
                        if ch != b' ' || bg != COLOR_BG {
                            render_glyph(&mut self.backend, x, y, ch, fg, bg);
                        }
                    }
                }
            } else {
                // Render from current screen
                let screen_y = y - offset.min(y);
                if screen_y < rows {
                    for x in 0..cols {
                        let idx = screen_y * cols + x;
                        let ch = self.vt_screens[vt_idx].cells[idx];
                        let fg = self.vt_screens[vt_idx].fg_cells[idx];
                        let bg = self.vt_screens[vt_idx].bg_cells[idx];
                        if ch != b' ' || bg != COLOR_BG {
                            render_glyph(&mut self.backend, x, y, ch, fg, bg);
                        }
                    }
                }
            }
        }

        // Draw cursor only when viewing live (offset == 0)
        if offset == 0 && self.vt_screens[vt_idx].cursor_visible {
            let cx = self.vt_screens[vt_idx].cursor_x;
            let cy = self.vt_screens[vt_idx].cursor_y;
            let cidx = cy * self.cols + cx;
            if cidx < self.vt_screens[vt_idx].cells.len() {
                let ch = self.vt_screens[vt_idx].cells[cidx];
                let fg = self.vt_screens[vt_idx].fg_cells[cidx];
                let bg = self.vt_screens[vt_idx].bg_cells[cidx];
                render_cursor_block(&mut self.backend, cx, cy, ch, fg, bg);
            }
        }

        self.last_cursor_x = self.vt_screens[vt_idx].cursor_x;
        self.last_cursor_y = self.vt_screens[vt_idx].cursor_y;
    }

    /// Render dirty cells from the active VtScreen to the framebuffer.
    fn flush_dirty_cells(&mut self) {
        let vt = self.active_vt;
        let cols = self.cols;
        let dirty = core::mem::take(&mut self.vt_screens[vt].dirty_cells);
        for &(x, y) in &dirty {
            let idx = y * cols + x;
            let ch = self.vt_screens[vt].cells[idx];
            let fg = self.vt_screens[vt].fg_cells[idx];
            let bg = self.vt_screens[vt].bg_cells[idx];
            render_glyph(&mut self.backend, x, y, ch, fg, bg);
        }
    }

    /// Update the cursor overlay on the framebuffer.
    fn redraw_cursor(&mut self) {
        let vt = self.active_vt;
        let cols = self.cols;
        let cx = self.vt_screens[vt].cursor_x;
        let cy = self.vt_screens[vt].cursor_y;

        // 1) Erase old cursor if it moved.
        if self.last_cursor_x != cx || self.last_cursor_y != cy {
            let old_idx = self.last_cursor_y * cols + self.last_cursor_x;
            if old_idx < self.vt_screens[vt].cells.len() {
                let ch = self.vt_screens[vt].cells[old_idx];
                let fg = self.vt_screens[vt].fg_cells[old_idx];
                let bg = self.vt_screens[vt].bg_cells[old_idx];
                render_glyph(
                    &mut self.backend,
                    self.last_cursor_x,
                    self.last_cursor_y,
                    ch,
                    fg,
                    bg,
                );
            }
        }

        // 2) Repaint current cell (clears any old cursor block there).
        let idx = cy * cols + cx;
        if idx < self.vt_screens[vt].cells.len() {
            let ch = self.vt_screens[vt].cells[idx];
            let fg = self.vt_screens[vt].fg_cells[idx];
            let bg = self.vt_screens[vt].bg_cells[idx];
            render_glyph(&mut self.backend, cx, cy, ch, fg, bg);
        }

        // 3) Draw cursor block if visible.
        if self.vt_screens[vt].cursor_visible {
            let ch = self.vt_screens[vt].cells[idx];
            let fg = self.vt_screens[vt].fg_cells[idx];
            let bg = self.vt_screens[vt].bg_cells[idx];
            render_cursor_block(&mut self.backend, cx, cy, ch, fg, bg);
        }

        // 4) Update last cursor position.
        self.last_cursor_x = cx;
        self.last_cursor_y = cy;
    }
}

// ── Free rendering helpers ─────────────────────────────────────────────

/// Render a glyph bitmap to the backend at grid position (x, y).
/// Uses the static byte-mask LUT + SSE2 blend_row for non-shade glyphs.
fn render_glyph<B: ConsoleBackend>(
    backend: &mut B,
    x: usize,
    y: usize,
    ch: u8,
    fg: u32,
    bg: u32,
) {
    if let Some(glyph) = shade_glyph(ch) {
        let mut row_buffer = [0u32; GLYPH_W];
        for (row, line) in glyph.iter().enumerate() {
            for (col, pixel) in row_buffer.iter_mut().enumerate().take(GLYPH_W) {
                let bit = (line >> (7 - col)) & 1;
                *pixel = if bit != 0 { fg } else { bg };
            }
            backend.put_pixels_row(x * GLYPH_W, y * GLYPH_H + row, &row_buffer);
        }
        return;
    }

    let glyph = font_glyph(ch);
    let px = x * GLYPH_W;
    let py = y * GLYPH_H;
    let mut row_buffer = [0u32; GLYPH_W];
    for (row, line) in glyph.iter().enumerate() {
        let mask = libcluu::atlas::mask_for_byte(*line);
        libcluu::simd::blend_row(mask, fg, bg, &mut row_buffer);
        backend.put_pixels_row(px, py + row, &row_buffer);
    }
}

/// Draw a full-cell block cursor: redraw the cell glyph with fg/bg swapped
/// so the character remains visible inside the cursor.
fn render_cursor_block<B: ConsoleBackend>(
    backend: &mut B,
    x: usize,
    y: usize,
    ch: u8,
    fg: u32,
    bg: u32,
) {
    render_glyph(backend, x, y, ch, bg, fg);
}

/// Load the glyph bitmap for a single CP437 byte value.
fn font_glyph(ch: u8) -> [u8; GLYPH_H] {
    if let Some(glyph) = shade_glyph(ch) {
        return glyph;
    }
    libcluu::font::glyph_for_cp437(ch)
}

fn shade_glyph(ch: u8) -> Option<[u8; GLYPH_H]> {
    match ch {
        0xDB => Some([0xFF; GLYPH_H]),        // full block
        0xB0 => Some(make_shade(0x88, 0x22)), // light shade
        0xB1 => Some(make_shade(0xAA, 0x55)), // medium shade
        0xB2 => Some(make_shade(0xEE, 0x77)), // dark shade
        _ => None,
    }
}

fn make_shade(a: u8, b: u8) -> [u8; GLYPH_H] {
    let mut glyph = [0u8; GLYPH_H];

    for (row, cell) in glyph.iter_mut().enumerate() {
        *cell = if row % 2 == 0 { a } else { b };
    }

    glyph
}

/// Map a Unicode codepoint to the closest CP437 glyph index.
///
/// Covers ASCII, Latin-1 supplement, box drawing, block elements,
/// Greek letters, and common math/currency symbols.
fn unicode_to_cp437(cp: u32) -> u8 {
    match cp {
        // ASCII — direct mapping
        0x0000..=0x007F => cp as u8,

        // Latin-1 supplement → CP437 extended
        0x00C7 => 0x80, // Ç
        0x00FC => 0x81, // ü
        0x00E9 => 0x82, // é
        0x00E2 => 0x83, // â
        0x00E4 => 0x84, // ä
        0x00E0 => 0x85, // à
        0x00E5 => 0x86, // å
        0x00E7 => 0x87, // ç
        0x00EA => 0x88, // ê
        0x00EB => 0x89, // ë
        0x00E8 => 0x8A, // è
        0x00EF => 0x8B, // ï
        0x00EE => 0x8C, // î
        0x00EC => 0x8D, // ì
        0x00C4 => 0x8E, // Ä
        0x00C5 => 0x8F, // Å
        0x00C9 => 0x90, // É
        0x00E6 => 0x91, // æ
        0x00C6 => 0x92, // Æ
        0x00F4 => 0x93, // ô
        0x00F6 => 0x94, // ö
        0x00F2 => 0x95, // ò
        0x00FB => 0x96, // û
        0x00F9 => 0x97, // ù
        0x00FF => 0x98, // ÿ
        0x00D6 => 0x99, // Ö
        0x00DC => 0x9A, // Ü
        0x00A2 => 0x9B, // ¢
        0x00A3 => 0x9C, // £
        0x00A5 => 0x9D, // ¥
        0x00AA => 0xA6, // ª
        0x00BA => 0xA7, // º
        0x00BF => 0xA8, // ¿
        0x00AC => 0xAA, // ¬
        0x00BD => 0xAB, // ½
        0x00BC => 0xAC, // ¼
        0x00A1 => 0xAD, // ¡
        0x00AB => 0xAE, // «
        0x00BB => 0xAF, // »
        0x00C1 => 0xA0, // Á (approx)
        0x00ED => 0xA1, // í
        0x00F3 => 0xA3, // ó
        0x00FA => 0xA4, // ú
        0x00F1 => 0xA5, // ñ
        0x00D1 => 0xA5, // Ñ (same glyph)

        // Currency / special
        0x20A7 => 0x9E, // ₧ (peseta)
        0x0192 => 0x9F, // ƒ (florin)

        // Block elements
        0x2591 => 0xB0, // ░ light shade
        0x2592 => 0xB1, // ▒ medium shade
        0x2593 => 0xB2, // ▓ dark shade
        0x2588 => 0xDB, // █ full block
        0x2584 => 0xDC, // ▄ lower half
        0x258C => 0xDD, // ▌ left half
        0x2590 => 0xDE, // ▐ right half
        0x2580 => 0xDF, // ▀ upper half

        // Box drawing — single lines
        0x2502 => 0xB3, // │
        0x2524 => 0xB4, // ┤
        0x2510 => 0xBF, // ┐
        0x2514 => 0xC0, // └
        0x2534 => 0xC1, // ┴
        0x252C => 0xC2, // ┬
        0x251C => 0xC3, // ├
        0x2500 => 0xC4, // ─
        0x253C => 0xC5, // ┼
        0x2518 => 0xD9, // ┘
        0x250C => 0xDA, // ┌

        // Box drawing — double lines
        0x2551 => 0xBA, // ║
        0x2557 => 0xBB, // ╗
        0x255D => 0xBC, // ╝
        0x255A => 0xC8, // ╚
        0x2554 => 0xC9, // ╔
        0x2569 => 0xCA, // ╩
        0x2566 => 0xCB, // ╦
        0x2560 => 0xCC, // ╠
        0x2550 => 0xCD, // ═
        0x256C => 0xCE, // ╬

        // Box drawing — mixed single/double
        0x2561 => 0xB5, // ╡
        0x2562 => 0xB6, // ╢
        0x2556 => 0xB7, // ╖
        0x2555 => 0xB8, // ╕
        0x2563 => 0xB9, // ╣
        0x2558 => 0xBD, // ╘
        0x2559 => 0xBE, // ╙
        0x255C => 0xB7, // ╜ (approx ╖)
        0x255B => 0xB8, // ╛ (approx ╕)
        0x2564 => 0xD1, // ╤ (approx)
        0x2565 => 0xD2, // ╥ (approx)
        0x2567 => 0xCF, // ╧ (approx)
        0x2568 => 0xD0, // ╨ (approx)

        // Greek letters
        0x0391 | 0x03B1 => 0xE0,          // Α/α → α
        0x0392 | 0x03B2 | 0x00DF => 0xE1, // Β/β/ß → ß
        0x0393 => 0xE2,                   // Γ
        0x03C0 => 0xE3,                   // π
        0x03A3 => 0xE4,                   // Σ
        0x03C3 => 0xE5,                   // σ
        0x03BC | 0x00B5 => 0xE6,          // μ/µ
        0x03C4 => 0xE7,                   // τ
        0x03A6 | 0x03C6 => 0xE8,          // Φ/φ
        0x0398 | 0x03B8 => 0xE9,          // Θ/θ
        0x03A9 | 0x03C9 => 0xEA,          // Ω/ω
        0x03B4 => 0xEB,                   // δ
        0x03B5 => 0xEE,                   // ε

        // Math symbols
        0x221E => 0xEC, // ∞
        0x2208 => 0xEE, // ∈ (approx ε)
        0x2229 => 0xEF, // ∩
        0x2261 => 0xF0, // ≡
        0x00B1 => 0xF1, // ±
        0x2265 => 0xF2, // ≥
        0x2264 => 0xF3, // ≤
        0x2320 => 0xF4, // ⌠
        0x2321 => 0xF5, // ⌡
        0x00F7 => 0xF6, // ÷
        0x2248 => 0xF7, // ≈
        0x00B0 => 0xF8, // °
        0x2219 => 0xF9, // ∙
        0x00B7 => 0xFA, // ·
        0x221A => 0xFB, // √
        0x207F => 0xFC, // ⁿ
        0x00B2 => 0xFD, // ²
        0x25A0 => 0xFE, // ■
        0x00A0 => 0xFF, // NBSP → CP437 0xFF

        // CLUU private-use range for Tier-3 rounded corner sub-cells.
        // Now centralized in libcluu::font.
        0xE000..=0xE00F => 0xF0u8 + (cp - 0xE000) as u8,

        // Unmapped codepoint
        _ => b'?',
    }
}
