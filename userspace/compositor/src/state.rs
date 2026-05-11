//! Compositor core state types and palette init.
//!
//! `Compositor` is the long-lived owner of the framebuffer mapping,
//! cell grid, window list, and IPC token table. `Window` describes one
//! tenant's region + SHM mapping. `WindowShm` is the on-the-wire header
//! that lives at the start of each per-window shared region (32 bytes,
//! cells follow).

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub type WindowId = u64;

/// Axis-aligned pixel rectangle used to track the dirty region of the
/// backbuffer.  Coordinates are in pixels, origin at top-left.
#[derive(Debug, Clone, Copy)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl PixelRect {
    /// Return the smallest rect that contains both `self` and `other`.
    pub fn extend(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.w).max(other.x + other.w);
        let bot   = (self.y + self.h).max(other.y + other.h);
        Self { x, y, w: right - x, h: bot - y }
    }
}

/// Per-task next-fire deadlines, absolute monotonic milliseconds.
///
/// `u64::MAX` means "task currently inactive — never fire". Each task
/// self-resets its own deadline after firing.
pub struct Deadlines {
    /// Next frame-flush deadline. Set to `now + MIN_FRAME_MS` after a flush.
    /// Set to `u64::MAX` when no dirty cells pending OR compositor inactive.
    pub next_frame_ms: u64,

    /// Next status-bar clock update deadline. Set to `now + 1000` after each
    /// clock tick.
    pub next_clock_ms: u64,
}

impl Deadlines {
    pub const fn new() -> Self {
        Self {
            next_frame_ms: u64::MAX,
            next_clock_ms: 0, // fire immediately on first iteration
        }
    }

    /// Time in ms until the earliest deadline. Saturates to 0 if any deadline
    /// is already due.
    pub fn next_timeout_ms(&self, now_ms: u64) -> u64 {
        let next = self.next_frame_ms.min(self.next_clock_ms);
        next.saturating_sub(now_ms)
    }
}

/// On-the-wire SHM region header. Layout MUST stay stable across
/// compositor + client crates — both projects copy this definition.
/// The cells (`u64` per cell, `width * height` of them) follow at byte
/// offset 32, contiguous in the same SHM region.
#[repr(C)]
pub struct WindowShm {
    pub magic: u32,
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub cursor_x: u32,
    pub cursor_y: u32,
    pub cursor_visible: u32,
    pub generation: u32,
}

pub const WIN_SHM_MAGIC: u32 = 0x57494e44; // "WIND"
pub const WIN_SHM_VERSION: u32 = 1;

/// Compositor's view of one tenant window.
pub struct Window {
    pub id: WindowId,
    pub owner_pid: u32,
    pub title: String,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub shm_va: *mut u8,
    pub shm_token: u64,
    pub shm_size: usize,
    pub last_gen: u32,
    pub input_endpoint: usize,
}

/// Long-lived compositor state. Single instance per process.
pub struct Compositor {
    pub fb_ptr: *mut u8,
    pub fb_phys: u64,
    pub fb_size: usize,
    pub width_px: u32,
    pub height_px: u32,
    pub pitch: u32,

    pub cols: u16,
    pub rows: u16,
    pub cell_grid: Vec<u64>,
    pub prev_cell_grid: Vec<u64>,
    pub cell_dirty: Vec<(u16, u16)>,

    pub palette: [u32; 256],
    pub backbuf: Vec<u32>,

    /// Pixel-level bounding box of cells blitted since the last fb flush.
    /// `None` means nothing was redrawn; `flush_backbuf_to_fb` is a no-op.
    pub dirty_rect: Option<PixelRect>,

    pub windows: Vec<Window>,
    pub focused: Option<WindowId>,
    pub active: bool,
    pub next_id: u64,

    pub clock_seconds: u64,

    /// Monotonic millisecond timestamp of the last flush+broadcast.
    /// Updated by `tick_frame` after each successful flush.
    pub last_flush_at: u64,

    /// Per-task deadline table for the event loop.
    pub deadlines: Deadlines,

    // Registry + IPC endpoints — filled in by main after registry::init().
    pub instance_id: u64,
    pub client_endpoint: usize,
    pub input_endpoint_global: usize,
    pub control_endpoint: usize,
    pub registry_endpoint: usize,
}

use libcluu::posix::{
    _close, _open, _read, mmap, c_void, O_RDWR, MAP_SHARED, PROT_READ, PROT_WRITE,
};
use libcluu::{Error, Result};

pub const GLYPH_W: u32 = 8;
pub const GLYPH_H: u32 = 16;

const FB_HEADER_MAGIC: u32 = 0x4642_4630; // "FB0\0"

impl Compositor {
    /// Open `/dev/fb0`, read the 40-byte geometry header, mmap the
    /// framebuffer write-combined, then close the fd.  Allocates
    /// `cell_grid` and `backbuf` from the dimensions returned by the header.
    pub fn init() -> Result<Self> {
        // 1. Open /dev/fb0
        let path = b"/dev/fb0\0";
        let fd = unsafe { _open(path.as_ptr() as *const i8, O_RDWR, 0) };
        if fd < 0 {
            return Err(Error::NotFound);
        }

        // 2. Read 40-byte header
        let mut hdr = [0u8; 40];
        let n = unsafe { _read(fd, hdr.as_mut_ptr() as *mut c_void, 40) };
        if n != 40 {
            unsafe { _close(fd); }
            return Err(Error::InvalidArgument);
        }
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        if magic != FB_HEADER_MAGIC {
            unsafe { _close(fd); }
            return Err(Error::InvalidArgument);
        }
        let width_px  = u32::from_le_bytes([hdr[ 4], hdr[ 5], hdr[ 6], hdr[ 7]]);
        let height_px = u32::from_le_bytes([hdr[ 8], hdr[ 9], hdr[10], hdr[11]]);
        let pitch     = u32::from_le_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]);
        // hdr[16..20] = bpp (unused by compositor, framebuffer is always 32bpp)
        // hdr[20..24] = reserved
        let fb_size = u64::from_le_bytes([
            hdr[24], hdr[25], hdr[26], hdr[27],
            hdr[28], hdr[29], hdr[30], hdr[31],
        ]) as usize;
        let fb_phys = u64::from_le_bytes([
            hdr[32], hdr[33], hdr[34], hdr[35],
            hdr[36], hdr[37], hdr[38], hdr[39],
        ]);

        // 3. mmap — libcluu::posix::mmap detects the FB magic and routes to
        //    MAP_DEVICE_WC automatically for MAP_SHARED + /dev/fb0 fds.
        let mapped = unsafe {
            mmap(
                core::ptr::null_mut::<c_void>(),
                fb_size,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd,
                0,
            )
        };

        // 4. Close fd — mmap holds its own reference via the kernel mapping.
        unsafe { _close(fd); }

        if mapped as isize == -1 || mapped.is_null() {
            return Err(Error::InvalidArgument);
        }
        let fb_ptr = mapped as *mut u8;

        let cols = (width_px / GLYPH_W) as u16;
        let rows = (height_px / GLYPH_H) as u16;
        let cell_count  = cols as usize * rows as usize;
        let pixel_count = (width_px * height_px) as usize;

        Ok(Self {
            fb_ptr,
            fb_phys,
            fb_size,
            width_px,
            height_px,
            pitch,
            cols,
            rows,
            cell_grid: alloc::vec![0u64; cell_count],
            prev_cell_grid: alloc::vec![u64::MAX; cell_count],
            cell_dirty: Vec::new(),
            palette: xterm_256_palette(),
            backbuf: alloc::vec![0u32; pixel_count],
            dirty_rect: None,
            windows: Vec::new(),
            focused: None,
            active: false,
            next_id: 1,
            clock_seconds: 0,
            last_flush_at: 0,
            deadlines: Deadlines::new(),
            instance_id: 0,
            client_endpoint: 0,
            input_endpoint_global: 0,
            control_endpoint: 0,
            registry_endpoint: 0,
        })
    }
}

impl Compositor {
    /// Allocate a window per the request. Returns
    /// `(id, frame_token, granted_w, granted_h)` on success.
    ///
    /// Granted dims are clamped to the screen minus row 0 (status bar).
    /// `owner_pid` is the authenticated sender's tid (CLUU does not yet
    /// distinguish tid from pid for one-thread apps).
    /// `input_endpoint` is the app's long-lived endpoint for FRAME_READY and INPUT_FORWARD.
    pub fn handle_win_register(
        &mut self,
        owner_pid: u32,
        req_w: u32,
        req_h: u32,
        title: &str,
        input_endpoint: usize,
    ) -> Result<(WindowId, u64, u32, u32)> {
        let granted_w = (req_w as u16).min(self.cols);
        let granted_h = (req_h as u16).min(self.rows.saturating_sub(1));
        // Minimum 3×3: 1-cell chrome on each side + at least 1 interior cell.
        if granted_w < 3 || granted_h < 3 {
            return Err(libcluu::Error::InvalidArgument);
        }

        let cells_bytes = granted_w as usize * granted_h as usize * 8;
        let header_bytes = core::mem::size_of::<WindowShm>();
        let total_bytes = header_bytes + cells_bytes;
        let (token, allocated) = crate::shm::alloc_frame(total_bytes)?;

        let id = self.next_id;
        self.next_id += 1;

        // Per-window VA slot, well above APP_FB_BASE. Each id reserves a
        // 4 MiB stride so neighbouring windows never collide regardless of
        // their pixel dimensions. 256 MiB region total before we run out.
        let va_base: usize = 0xC100_0000;
        let va = va_base + (id as usize) * 0x40_0000;
        crate::shm::map_frame_rw(va, token, allocated)?;

        unsafe {
            let hdr = va as *mut WindowShm;
            (*hdr).magic = WIN_SHM_MAGIC;
            (*hdr).version = WIN_SHM_VERSION;
            (*hdr).width = granted_w as u32;
            (*hdr).height = granted_h as u32;
            (*hdr).cursor_x = 0;
            (*hdr).cursor_y = 0;
            (*hdr).cursor_visible = 0;
            (*hdr).generation = 0;
            // Zero cell area
            let cells_ptr = (va + header_bytes) as *mut u8;
            core::ptr::write_bytes(cells_ptr, 0, cells_bytes);
        }

        // Cascade window placement. Status bar reserves row 0, so y >= 1.
        let offset = (id as u16) * 2;
        let max_x = self.cols.saturating_sub(granted_w);
        let max_y = self.rows.saturating_sub(granted_h);
        let x = offset.min(max_x);
        let y = (1 + offset).min(max_y.max(1));

        let mut title_owned = alloc::string::String::new();
        title_owned.push_str(title);
        if title_owned.len() > 31 {
            title_owned.truncate(31);
        }

        self.windows.push(Window {
            id,
            owner_pid,
            title: title_owned,
            x,
            y,
            w: granted_w,
            h: granted_h,
            shm_va: va as *mut u8,
            shm_token: token,
            shm_size: allocated,
            last_gen: 0,
            input_endpoint,
        });
        self.focused = Some(id);
        // Mark all the window's cells dirty so the (eventual) compose pass
        // emits chrome + interior.
        for cy in y..y.saturating_add(granted_h) {
            for cx in x..x.saturating_add(granted_w) {
                self.cell_dirty.push((cx, cy));
            }
        }
        Ok((id, token, granted_w as u32, granted_h as u32))
    }
}

impl Compositor {
    /// App says "I redrew (x,y,w,h) inside my window's interior". Mark
    /// the corresponding total-grid cells dirty.
    ///
    /// Chrome is 1 cell on each side, so interior starts at local (1,1).
    pub fn handle_win_damage(&mut self, id: WindowId, x: u32, y: u32, w: u32, h: u32) {
        let Some(win) = self.windows.iter().find(|w| w.id == id) else { return; };
        let inner_w = win.w.saturating_sub(2); // 1 chrome col each side
        let inner_h = win.h.saturating_sub(2); // 1 chrome row each side
        let cx0 = (x as u16).min(inner_w);
        let cy0 = (y as u16).min(inner_h);
        let cx1 = ((x as u16).saturating_add(w as u16)).min(inner_w);
        let cy1 = ((y as u16).saturating_add(h as u16)).min(inner_h);
        for iy in cy0..cy1 {
            for ix in cx0..cx1 {
                let gx = win.x + 1 + ix;
                let gy = win.y + 1 + iy;
                self.cell_dirty.push((gx, gy));
            }
        }
    }
}

impl Compositor {
    /// Update the title of a window and dirty the title row so chrome re-renders.
    pub fn handle_win_set_title(&mut self, id: WindowId, title: &str) {
        let win_idx = match self.windows.iter().position(|w| w.id == id) {
            Some(i) => i,
            None => return,
        };
        // Truncate to fit title strip (<=31 chars matches the storage cap
        // in handle_win_register).
        let safe = if title.len() > 31 { &title[..31] } else { title };
        self.windows[win_idx].title.clear();
        self.windows[win_idx].title.push_str(safe);
        let win = &self.windows[win_idx];
        // Title is in the top chrome row (ly=0), so global y = win.y.
        let title_y = win.y;
        for cx in win.x..win.x.saturating_add(win.w) {
            self.cell_dirty.push((cx, title_y));
        }
    }
}

impl Compositor {
    /// Cycle focus forward (Alt+Tab). The newly focused window is moved to the
    /// top of the z-order (end of the `windows` Vec) and the grid is fully
    /// dirtied so chrome repaints with the updated focus state.
    pub fn focus_next(&mut self) {
        if self.windows.is_empty() { return; }
        let cur = self.focused;
        let pos = cur
            .and_then(|id| self.windows.iter().position(|w| w.id == id))
            .unwrap_or(0);
        let new = (pos + 1) % self.windows.len();
        let win = self.windows.remove(new);
        let id = win.id;
        self.windows.push(win);
        self.focused = Some(id);
        self.repaint_all();
    }

    /// Cycle focus backward (Alt+Shift+Tab).
    pub fn focus_prev(&mut self) {
        if self.windows.is_empty() { return; }
        let len = self.windows.len();
        let pos = self.focused
            .and_then(|id| self.windows.iter().position(|w| w.id == id))
            .unwrap_or(0);
        let new = (pos + len - 1) % len;
        let win = self.windows.remove(new);
        let id = win.id;
        self.windows.push(win);
        self.focused = Some(id);
        self.repaint_all();
    }

    /// Move the focused window by (dx, dy) cells, clamped to screen bounds.
    /// Row 0 is the status bar; window top edge may not go above row 1.
    pub fn move_focused(&mut self, dx: i16, dy: i16) {
        let Some(id) = self.focused else { return; };
        let pos = match self.windows.iter().position(|w| w.id == id) {
            Some(p) => p,
            None => return,
        };
        let win = &self.windows[pos];
        let new_x = (win.x as i32 + dx as i32)
            .max(0)
            .min(self.cols as i32 - win.w as i32) as u16;
        let new_y = (win.y as i32 + dy as i32)
            .max(1)
            .min(self.rows as i32 - win.h as i32) as u16;
        let old_x = win.x;
        let old_y = win.y;
        let w = win.w;
        let h = win.h;
        self.windows[pos].x = new_x;
        self.windows[pos].y = new_y;
        // Dirty old and new footprints.
        for cy in old_y..old_y.saturating_add(h) {
            for cx in old_x..old_x.saturating_add(w) {
                self.cell_dirty.push((cx, cy));
            }
        }
        for cy in new_y..new_y.saturating_add(h) {
            for cx in new_x..new_x.saturating_add(w) {
                self.cell_dirty.push((cx, cy));
            }
        }
    }

    /// Resize the focused window by (dw, dh) cells.
    /// Minimum size is 5×5; maximum is clamped to the screen edge.
    pub fn resize_focused(&mut self, dw: i16, dh: i16) {
        let Some(id) = self.focused else { return; };
        let pos = match self.windows.iter().position(|w| w.id == id) {
            Some(p) => p,
            None => return,
        };
        let win = &self.windows[pos];
        let new_w = ((win.w as i32 + dw as i32)
            .max(3)
            .min(self.cols as i32 - win.x as i32)) as u16;
        let new_h = ((win.h as i32 + dh as i32)
            .max(3)
            .min(self.rows as i32 - win.y as i32)) as u16;
        let old_w = win.w;
        let old_h = win.h;
        let x = win.x;
        let y = win.y;
        self.windows[pos].w = new_w;
        self.windows[pos].h = new_h;
        // Dirty the union of old and new footprints.
        let max_w = old_w.max(new_w);
        let max_h = old_h.max(new_h);
        for cy in y..y.saturating_add(max_h) {
            for cx in x..x.saturating_add(max_w) {
                self.cell_dirty.push((cx, cy));
            }
        }
    }

    /// Mark every cell on screen dirty (used after focus changes so chrome
    /// repaints with correct focused/unfocused colours).
    pub fn repaint_all(&mut self) {
        for cy in 0..self.rows {
            for cx in 0..self.cols {
                self.cell_dirty.push((cx, cy));
            }
        }
    }
}

impl Compositor {
    /// VT switch: compositor's VT became active — resume fb writes.
    pub fn handle_vt_activate(&mut self) {
        self.active = true;
        self.repaint_all();
    }

    /// VT switch: compositor's VT became inactive — suppress fb writes.
    pub fn handle_vt_deactivate(&mut self) {
        self.active = false;
    }
}

impl Compositor {
    /// Forward a raw kbd event to the focused window's input endpoint.
    /// `ascii`/`mods`/`scancode`/`extended` come straight from the
    /// `KbdEvent` variant of `protocol::Incoming`.
    pub fn forward_input_event(&self, ascii: u8, mods: u8, scancode: u8, extended: u8) {
        let Some(id) = self.focused else { return; };
        let Some(win) = self.windows.iter().find(|w| w.id == id) else { return; };
        if win.input_endpoint == 0 { return; }
        let msg = libcluu::types::Message::new(
            libcluu::ipc::COMP_INPUT_FORWARD_LABEL,
            [
                id as usize,
                ascii as usize,
                mods as usize,
                scancode as usize,
                extended as usize,
                0usize, // kind = 0 → ordinary input
            ],
            6,
        );
        let _ = libcluu::ipc::send(win.input_endpoint, &msg, libcluu::types::IpcFlags::empty());
    }

    /// Send a close-request to the focused window's input endpoint.
    pub fn forward_close_request(&self) {
        let Some(id) = self.focused else { return; };
        let Some(win) = self.windows.iter().find(|w| w.id == id) else { return; };
        if win.input_endpoint == 0 { return; }
        let msg = libcluu::types::Message::new(
            libcluu::ipc::COMP_INPUT_FORWARD_LABEL,
            [id as usize, 0, 0, 0, 0, 99usize /* kind = 99 → close-request */],
            6,
        );
        let _ = libcluu::ipc::send(win.input_endpoint, &msg, libcluu::types::IpcFlags::empty());
    }
}

impl Compositor {
    /// Free the window's frame, drop it from the list, repaint covered cells.
    /// Called explicitly via WIN_DESTROY. Implicit destroy on owner-exit is
    /// deferred — would need procmgr to broadcast exits to non-spawner
    /// watchers (no such API today).
    pub fn handle_win_destroy(&mut self, id: WindowId) {
        let Some(pos) = self.windows.iter().position(|w| w.id == id) else {
            return;
        };
        let win = self.windows.remove(pos);
        let _ = crate::shm::free_frame(win.shm_token);
        // Mark covered cells dirty so the next compose pass repaints bg.
        for cy in win.y..win.y.saturating_add(win.h) {
            for cx in win.x..win.x.saturating_add(win.w) {
                self.cell_dirty.push((cx, cy));
            }
        }
        if self.focused == Some(id) {
            self.focused = self.windows.last().map(|w| w.id);
        }
    }
}

const MIN_FRAME_MS: u64 = 16;
const CLOCK_PERIOD_MS: u64 = 1000;

impl Compositor {
    /// Tick the frame deadline. If active + dirty cells pending and the
    /// deadline has passed, flush + broadcast + reset the deadline.
    /// Returns `true` if a flush happened (caller should broadcast).
    pub fn tick_frame(&mut self, now_ms: u64) -> bool {
        if !self.active {
            return false;
        }
        if self.cell_dirty.is_empty() && self.prev_cell_grid == self.cell_grid {
            // No work to do; let deadline sleep.
            self.deadlines.next_frame_ms = u64::MAX;
            return false;
        }
        if now_ms < self.deadlines.next_frame_ms {
            return false; // not yet
        }
        self.flush_grid_to_backbuf();
        self.flush_backbuf_to_fb();
        self.deadlines.next_frame_ms = now_ms + MIN_FRAME_MS;
        self.last_flush_at = now_ms;
        true
    }

    /// Tick the clock deadline. If due, refresh `clock_seconds`, dirty the
    /// status row, and reset the deadline.
    pub fn tick_clock(&mut self, now_ms: u64, now_secs: u64) {
        if now_ms < self.deadlines.next_clock_ms {
            return;
        }
        if now_secs != self.clock_seconds {
            self.clock_seconds = now_secs;
            for cx in 0..self.cols {
                self.cell_dirty.push((cx, 0));
            }
        }
        self.deadlines.next_clock_ms = now_ms + CLOCK_PERIOD_MS;
    }

    /// Set the frame deadline to now+MIN_FRAME_MS (next loop iteration flushes)
    /// when new dirty cells arrive. Idempotent — leaves existing deadline alone
    /// if already set.
    pub fn schedule_frame(&mut self, now_ms: u64) {
        if self.deadlines.next_frame_ms == u64::MAX {
            self.deadlines.next_frame_ms = now_ms.saturating_add(MIN_FRAME_MS);
        }
    }
}

impl Compositor {
    /// Glyph-blit cells whose value differs from `prev_cell_grid` and write
    /// the resulting pixels into `backbuf`. Caller must follow with
    /// `flush_backbuf_to_fb` to push to the framebuffer.
    pub fn flush_grid_to_backbuf(&mut self) {
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let pitch_words = self.width_px as usize; // contiguous backbuf
        let glyph_w = libcluu::atlas::GLYPH_W;
        let glyph_h = libcluu::atlas::GLYPH_H;

        for cy in 0..rows {
            for cx in 0..cols {
                let idx = cy * cols + cx;
                let cell = self.cell_grid[idx];
                if self.prev_cell_grid[idx] == cell {
                    continue;
                }
                self.prev_cell_grid[idx] = cell;

                // Extend the pixel dirty rect to cover this cell.
                let cell_pr = PixelRect {
                    x: (cx as u32) * glyph_w as u32,
                    y: (cy as u32) * glyph_h as u32,
                    w: glyph_w as u32,
                    h: glyph_h as u32,
                };
                self.dirty_rect = Some(match self.dirty_rect {
                    Some(prev) => prev.extend(cell_pr),
                    None => cell_pr,
                });

                let cp = (cell & 0x1F_FFFF) as u32;
                let fg_idx = ((cell >> 21) & 0xFF) as u8;
                let bg_idx = ((cell >> 29) & 0xFF) as u8;
                let _attrs = ((cell >> 37) & 0x07) as u8; // bold/etc later
                let fg = self.palette[fg_idx as usize];
                let bg = self.palette[bg_idx as usize];

                // Map Unicode → CP437 → font byte. Codepoints outside
                // BMP-ASCII / CP437 fall back to '?' via the helper.
                let ch = unicode_to_cp437(cp);
                let glyph = font_glyph(ch);

                let px = cx * glyph_w;
                let py = cy * glyph_h;
                let mut row_buffer = [0u32; 8];
                for row in 0..glyph_h {
                    let line = glyph[row];
                    let mask = libcluu::atlas::mask_for_byte(line);
                    libcluu::simd::blend_row(mask, fg, bg, &mut row_buffer);
                    let off = (py + row) * pitch_words + px;
                    self.backbuf[off..off + glyph_w].copy_from_slice(&row_buffer);
                }
            }
        }
    }

    /// Push only the dirty pixel rect from the backbuf to the framebuffer.
    ///
    /// Consumes and clears `dirty_rect` via `.take()`.  If `dirty_rect` is
    /// `None` (nothing changed since the last flush) the function returns
    /// immediately without touching the framebuffer.
    ///
    /// Rows are copied individually so only the changed columns within each
    /// dirty row are written, reducing WC write traffic by ~14× for a typical
    /// small TUI window on a full 1024×768 screen.
    ///
    /// No-op when `fb_ptr` is null (compositor spawned without FB mapping,
    /// e.g. shell-spawn in test harness before T24 wires up procmgr autostart).
    pub fn flush_backbuf_to_fb(&mut self) {
        if self.fb_ptr.is_null() { return; }
        let Some(rect) = self.dirty_rect.take() else { return; };

        // Clamp to screen bounds defensively.
        let x     = rect.x.min(self.width_px);
        let y     = rect.y.min(self.height_px);
        let right = (rect.x + rect.w).min(self.width_px);
        let bot   = (rect.y + rect.h).min(self.height_px);
        if right <= x || bot <= y { return; }
        let w = right - x;
        let h = bot - y;

        let pitch_words   = self.width_px as usize; // backbuf stride (u32 words)
        let bytes_per_row = (w as usize) * 4;

        for row in 0..(h as usize) {
            let py      = y as usize + row;
            let src_off = py * pitch_words + x as usize;
            // fb uses the hardware pitch (may differ from width_px * 4).
            let dst_off_bytes = py * (self.pitch as usize) + (x as usize) * 4;
            unsafe {
                let src = self.backbuf.as_ptr().add(src_off) as *const u8;
                let dst = self.fb_ptr.add(dst_off_bytes);
                core::ptr::copy_nonoverlapping(src, dst, bytes_per_row);
            }
        }
    }
}

// Local helpers.
fn font_glyph(ch: u8) -> [u8; 16] {
    libcluu::font::glyph_for_cp437(ch)
}

fn unicode_to_cp437(cp: u32) -> u8 {
    match cp {
        0x0000..=0x007F => cp as u8,
        0x2500 => 0xC4,                 // ─
        0x2502 => 0xB3,                 // │
        0x2550 => 0xCD,                 // ═
        0x2551 => 0xBA,                 // ║
        // Sharp box-drawing corners (CP437 standard).
        0x250C => 0xDA,                 // ┌
        0x2510 => 0xBF,                 // ┐
        0x2514 => 0xC0,                 // └
        0x2518 => 0xD9,                 // ┘
        _ => b'?',
    }
}

/// Build a standard xterm-256 ARGB palette.
///
/// 0..16  : ANSI base colours
/// 16..232: 6×6×6 RGB cube (rgb levels 0,95,135,175,215,255)
/// 232..256: 24-step grayscale ramp
pub fn xterm_256_palette() -> [u32; 256] {
    let mut p = [0u32; 256];
    let basic: [u32; 16] = [
        0x000000, 0x800000, 0x008000, 0x808000,
        0x000080, 0x800080, 0x008080, 0xC0C0C0,
        0x808080, 0xFF0000, 0x00FF00, 0xFFFF00,
        0x0000FF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
    ];
    for i in 0..16 {
        p[i] = 0xFF00_0000 | basic[i];
    }
    for i in 0..216 {
        let r = (i / 36) % 6;
        let g = (i / 6) % 6;
        let b = i % 6;
        let to8 = |c: usize| -> u32 {
            if c == 0 { 0 } else { (c as u32) * 40 + 55 }
        };
        p[16 + i] = 0xFF00_0000 | (to8(r) << 16) | (to8(g) << 8) | to8(b);
    }
    for i in 0..24 {
        let v = 8 + (i as u32) * 10;
        p[232 + i] = 0xFF00_0000 | (v << 16) | (v << 8) | v;
    }
    p
}
