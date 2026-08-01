//! Compositor core state types and init.
//!
//! `Compositor` is the long-lived owner of the cell grid, window list, and
//! IPC token table.  It is a client of displayd — no framebuffer mapping.
//! The compositor rasterizes TUI cells into a local backbuffer, copies the
//! dirty region into a shared frame token, and commits it to displayd via
//! `DISPLAY_BUFFER_COMMIT_LABEL`.  displayd owns the hardware output.
//!
//! Window lifecycle, focus management, input forwarding → `window_mgr`
//! Render pipeline, timing                               → `render`

extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use cluu_wire::display::{
    DISPLAY_OUTPUT_INFO_LABEL, DISPLAY_SET_GEOMETRY_LABEL,
    DISPLAY_SURFACE_CREATE_LABEL,
};
use libcluu::ipc;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry, Error, Result};

/// Opaque identifier for a compositor-managed window.
///
/// Assigned monotonically from `Compositor::next_id` and returned to the
/// registering client in the `WIN_REGISTER_REPLY` message. Clients use it
/// as an argument in `WIN_DAMAGE`, `WIN_DESTROY`, and `WIN_SET_TITLE`.
///
/// Note: still a plain `u64` alias — a newtype refactor is deferred (T52-A)
/// because the churn-to-value ratio is low relative to the current codebase size.
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

/// Bounded, deduplicated cell work queue.
pub struct DirtyCells {
    cols: u16,
    rows: u16,
    cells: Vec<(u16, u16)>,
    marked: Vec<u64>,
}

impl DirtyCells {
    pub fn new(cols: u16, rows: u16) -> Self {
        let cell_count = cols as usize * rows as usize;
        let word_count = cell_count.div_ceil(u64::BITS as usize);
        Self {
            cols,
            rows,
            cells: Vec::new(),
            marked: alloc::vec![0; word_count],
        }
    }

    pub fn push(&mut self, cell: (u16, u16)) {
        let (cx, cy) = cell;
        if cx >= self.cols || cy >= self.rows {
            return;
        }
        let index = cy as usize * self.cols as usize + cx as usize;
        let word_index = index / u64::BITS as usize;
        let bit = 1u64 << (index % u64::BITS as usize);
        if self.marked[word_index] & bit != 0 {
            return;
        }
        self.marked[word_index] |= bit;
        self.cells.push(cell);
    }

    pub fn pop(&mut self) -> Option<(u16, u16)> {
        let cell = self.cells.pop()?;
        let index = cell.1 as usize * self.cols as usize + cell.0 as usize;
        let word_index = index / u64::BITS as usize;
        self.marked[word_index] &= !(1u64 << (index % u64::BITS as usize));
        Some(cell)
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn intersects_rect(&self, x0: u16, y0: u16, x1: u16, y1: u16) -> bool {
        self.cells
            .iter()
            .any(|&(cx, cy)| cx >= x0 && cx < x1 && cy >= y0 && cy < y1)
    }

    pub fn reset(&mut self, cols: u16, rows: u16) {
        *self = Self::new(cols, rows);
    }

    pub fn mark_all(&mut self) {
        for cy in 0..self.rows {
            for cx in 0..self.cols {
                self.push((cx, cy));
            }
        }
    }
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
}

impl Deadlines {
    /// Create a `Deadlines` with the frame task parked at `u64::MAX`.
    /// All clock/blink wakeups are push-mode (timeserver TIME_TICK arrival)
    /// and do not use deadline-driven recv timeouts.
    pub const fn new() -> Self {
        Self {
            next_frame_ms: u64::MAX,
        }
    }

    /// Milliseconds until the next frame deadline, capped at `max_ms` to
    /// avoid passing near-`u64::MAX` values to the kernel recv syscall.
    ///
    /// When no frame is pending (`next_frame_ms == u64::MAX`) the loop
    /// blocks for up to `max_ms` before re-checking — equivalent to a
    /// bounded "block forever" that is safe to loop on Timeout.
    pub fn next_timeout_ms(&self, now_ms: u64, max_ms: u64) -> u64 {
        if self.next_frame_ms == u64::MAX {
            return max_ms;
        }
        self.next_frame_ms.saturating_sub(now_ms).min(max_ms)
    }
}

pub use libcluu::window_shm::{WindowShm, WIN_SHM_MAGIC, WIN_SHM_VERSION};

/// A pixel sub-region within a compositor text window.
///
/// Set via `COMP_WIN_SET_PIXEL_REGION_LABEL`. The compositor maps the
/// client-provided frame token and blits raw ARGB32 pixels to the backbuffer
/// for the cells covered by `cell_x..cell_x+cell_w`, `cell_y..cell_y+cell_h`,
/// skipping glyph-blit for those cells.
pub struct WindowPixelRegion {
    pub cell_x: u16,
    pub cell_y: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    pub pixel_w: u32,
    pub pixel_h: u32,
    pub mapping: ShmMapping,
    pub shm_token: u64,
}

impl WindowPixelRegion {
    /// True if cell `(cx, cy)` falls inside this pixel region.
    pub fn contains_cell(&self, cx: u16, cy: u16) -> bool {
        cx >= self.cell_x
            && cx < self.cell_x.saturating_add(self.cell_w)
            && cy >= self.cell_y
            && cy < self.cell_y.saturating_add(self.cell_h)
    }
}

/// A mapped SHM region shared between the compositor and one window client.
///
/// Wraps the raw `*mut u8` pointer so that all unsafe pointer arithmetic is
/// centralised here. Callers use the safe `header()` and `read_cell()`
/// accessors instead of scattering `unsafe` blocks across the codebase.
pub struct ShmMapping {
    ptr: core::ptr::NonNull<u8>,
    #[allow(dead_code)]
    // rationale: SHM mapping size retained for future bounds-checked accessors.
    pub size: usize,
}

impl ShmMapping {
    /// Construct from a raw virtual address + size.
    ///
    /// Returns `None` if `va == 0` (null pointer would be unsound).
    pub fn new(va: usize, size: usize) -> Option<Self> {
        core::ptr::NonNull::new(va as *mut u8).map(|ptr| Self { ptr, size })
    }

    /// Raw pointer to byte 0 of the mapping.
    ///
    /// Exposed for the kernel/syscall call-site in `window_mgr::handle_win_register`
    /// that initialises the WindowShm header immediately after mapping.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// Read-only reference to the `WindowShm` header at the base of the mapping.
    ///
    /// SAFETY: the mapping was established by `shm::map_frame_rw` with a size
    /// that includes at least `size_of::<WindowShm>()` bytes, and the region
    /// is never unmapped for the lifetime of this `ShmMapping`.
    pub fn header(&self) -> &WindowShm {
        // SAFETY: `self.ptr` is NonNull (checked in `new`). The mapping
        // established by `shm::map_frame_rw` is at least
        // `size_of::<WindowShm>()` bytes and remains valid for the
        // lifetime of `self`. `WindowShm` is `#[repr(C)]` and the SHM
        // page is page-aligned, satisfying alignment.
        unsafe { &*(self.ptr.as_ptr() as *const WindowShm) }
    }

    /// Read one packed cell at `(ix, iy)` from the cell array following the header.
    ///
    /// Returns `None` if the magic check fails or the coordinates are out of
    /// the bounds advertised in the header.
    pub fn read_cell(&self, ix: u16, iy: u16) -> Option<u64> {
        let hdr = self.header();
        if hdr.magic != WIN_SHM_MAGIC {
            return None;
        }
        let inner_w = hdr.width as u16;
        let inner_h = hdr.height as u16;
        if ix >= inner_w || iy >= inner_h {
            return None;
        }
        let header_size = core::mem::size_of::<WindowShm>();
        // SAFETY: `self.ptr` is a valid NonNull mapping. `header_size` is
        // `size_of::<WindowShm>()`, and the mapping includes at least that
        // plus the cell array (guaranteed by `shm::map_frame_rw`'s size
        // argument). The pointer arithmetic `.add(header_size)` stays within
        // the mapping because the cell array follows the header.
        let cells_ptr = unsafe { self.ptr.as_ptr().add(header_size) as *const u64 };
        let off = iy as usize * inner_w as usize + ix as usize;
        // SAFETY: `off = iy * inner_w + ix` with `ix < inner_w` and
        // `iy < inner_h` (both checked above), so `off < inner_w * inner_h`.
        // The cell array has `inner_w * inner_h` u64 entries (established
        // by the client during `WindowShm` init). `read_volatile` is used
        // because the client may update cells concurrently via SHM.
        Some(unsafe { core::ptr::read_volatile(cells_ptr.add(off)) })
    }
}

/// Compositor's view of one registered tenant window.
///
/// Created by `handle_win_register`, destroyed by `handle_win_destroy`.
/// Stored in `Compositor::windows` in z-order (last = top).
pub struct Window {
    /// Unique opaque identifier for this window.
    pub id: WindowId,
    /// TID of the registering thread (used as a pid surrogate until
    /// a real pid-from-tid lookup API exists — spec §10).
    #[allow(dead_code)]
    // rationale: owner_pid stored for future per-window kill-on-close.
    pub owner_pid: u32,
    /// Window title shown in the top chrome row.  At most 31 bytes.
    pub title: String,
    /// Top-left cell position in the compositor grid (column, row).
    pub x: u16,
    pub y: u16,
    /// Total window size in cells, *including* the 1-cell chrome border.
    pub w: u16,
    pub h: u16,
    /// Shared-memory mapping for this window's cell buffer.
    pub mapping: ShmMapping,
    /// Frame token handle used to unmap the SHM on destroy.
    pub shm_token: u64,
    /// Last observed `WindowShm::generation` (used to detect stale frames).
    pub last_gen: u32,
    /// Client endpoint for FRAME_READY + INPUT_FORWARD signals.
    /// 0 = legacy window that does not use the frame-callback protocol.
    pub input_endpoint: usize,
    /// Set when a WIN_DAMAGE event (or SHM generation advance) has been
    /// processed for this window since the last FRAME_READY broadcast.
    /// Cleared by `broadcast_frame_ready` after the message is sent.
    /// Windows with this flag clear are skipped in the broadcast, preventing
    /// the 60 Hz flood when the window hasn't rendered a new frame.
    pub pending_frame_ready: bool,
    /// When true: window covers the full cell grid (x=0, y=0, w=cols, h=rows).
    /// Compositor skips chrome rendering and status bar for this window while focused.
    pub fullscreen: bool,
    /// When true: no chrome (border/title) is drawn for this window.
    /// Interior cells map directly to SHM without chrome offset.
    pub no_chrome: bool,
    /// When true: window is modal. Pinned to z-top, grabs input, Esc dismisses.
    pub modal: bool,
    /// Session that owns this window, if any. Set on session handoff;
    /// `None` for sessionless windows (e.g. login modal, demo shells).
    pub session_id: Option<u32>,
    /// Optional pixel sub-region. When set, cells inside the region are
    /// blitted as raw ARGB32 pixels from SHM instead of glyph-blitted.
    pub pixel_region: Option<WindowPixelRegion>,
}

/// Long-lived compositor state.  Single instance per process, owned by `main`.
///
/// initialised by `Compositor::init` (connects to displayd, creates a surface,
/// allocates a backbuffer frame token).  All other fields are populated by
/// `main` after registry init.
///
/// Methods are split across `window_mgr` (window lifecycle + input) and
/// `render` (glyph blit, displayd commit, frame/clock timing).
pub struct Compositor {
    pub displayd_ep: usize,
    pub surface_token: u64,
    pub width_px: u32,
    pub height_px: u32,
    pub pitch: u32,

    pub fb_ptr: *mut u32,
    pub fb_grant_va: usize,
    pub fb_backing: Option<Vec<u32>>,

    /// Screen dimensions in cell units.
    pub cols: u16,
    pub rows: u16,
    /// Current composed cell grid (cols×rows packed u64 cells).
    pub cell_grid: Vec<u64>,
    /// Shadow copy of the last-flushed grid; used to skip unchanged cells.
    pub prev_cell_grid: Vec<u64>,
    /// Cells marked dirty since the last compose pass; drained by `recompute_dirty`.
    pub cell_dirty: DirtyCells,
    /// Recomputed cells awaiting glyph blitting; drained by `flush_grid_to_backbuf`.
    pub render_dirty: DirtyCells,

    pub palette: [u32; 256],

    /// Pixel-level bounding box of cells blitted since the last displayd commit.
    /// `None` means nothing was redrawn; `flush_backbuf_to_displayd` is a no-op.
    pub dirty_rect: Option<PixelRect>,

    /// Window list in z-order; last entry is the topmost (focused) window.
    pub windows: Vec<Window>,
    /// `WindowId` of the currently focused window, or `None` if no windows exist.
    pub focused: Option<WindowId>,
    /// Always true — displayd owns the VT; compositor is always active.
    pub active: bool,
    /// Monotonically increasing counter used to assign `WindowId`s.
    pub next_id: u64,

    /// Monotonic seconds from boot, refreshed by `tick_clock` each second.
    pub clock_seconds: u64,
    /// `true` once the timeserver endpoint is resolved and `clock_seconds`
    /// reflects a real timestamp. `false` while timeserver is pending;
    /// the status bar shows `--:--:--` in that state.
    pub clock_ready: bool,

    /// Last monotonic millisecond timestamp delivered by a TIME_TICK push message.
    /// Replaces per-iteration `clock_now_ms()` polling once push-mode is armed.
    /// Zero until the first tick arrives.
    pub last_clock_now_ms: u64,

    /// Monotonic millisecond timestamp of the last flush+broadcast.
    /// Updated by `tick_frame` after each successful flush.
    pub last_flush_at: u64,

    /// Per-task deadline table for the event loop.
    pub deadlines: Deadlines,

    // Registry + IPC endpoints — filled in by main after registry::init().
    /// Instance number (currently always 0; reserved for multi-compositor).
    pub instance_id: u64,
    /// Endpoint for `WIN_REGISTER`, `WIN_DAMAGE`, `WIN_DESTROY`, etc.
    pub client_endpoint: usize,
    /// Global input endpoint (kbd driver sends raw key events here).
    pub input_endpoint_global: usize,
    /// Control endpoint for VT activate/deactivate signals from vtmgr.
    pub control_endpoint: usize,
    /// Registry endpoint for forwarding grant-request control messages.
    pub registry_endpoint: usize,
    /// compositor no longer distinguishes system/user mode.
    /// Task 9, Plan 3: session lifecycle refactor.

    /// Set of session IDs for which the compositor has an active
    /// SESSION_ENDED subscription. Used to deduplicate subscriptions
    /// and verify incoming SESSION_ENDED events.
    pub tracked_sessions: BTreeSet<u32>,

    pub pointer_x: i32,
    pub pointer_y: i32,
    pub pointer_buttons: u8,
    pub drag_state: Option<DragState>,
    pub cursor_needs_render: bool,

    pub pixel_dirty: bool,

    /// TSC value at the last frame flush. Used by the throttle in
    /// `tick_frame` to limit frame rate during high-frequency mouse
    /// events. Raw TSC is monotonic and cheap (single `rdtsc`), unlike
    /// `last_clock_now_ms` which only advances on 1 Hz TIME_TICK.
    pub last_flush_tsc: u64,
    /// Calibrated TSC frequency in Hz, queried once at init via
    /// `clock_frequency`. Converts raw TSC deltas to milliseconds for
    /// the throttle comparison.
    pub tsc_freq_hz: u64,

}

#[derive(Clone, Copy)]
pub enum DragMode {
    Move,
    Resize,
}

#[derive(Clone, Copy)]
pub struct DragState {
    pub window_id: WindowId,
    pub mode: DragMode,
    pub start_cell_x: u16,
    pub start_cell_y: u16,
    pub start_win_x: u16,
    pub start_win_y: u16,
    pub start_win_w: u16,
    pub start_win_h: u16,
}

pub const GLYPH_W: u32 = 8;
pub const GLYPH_H: u32 = 16;

/// VAs for the compositor's double-buffered backbuf frame mappings
/// (pixel transfer to displayd).  Two frames are mapped so the compositor
/// can write to one while displayd copies from the other.
const COMP_FB_GRANT_VA: usize = 0x5600_0000;
const PAGE_SIZE: usize = 4096;

impl Compositor {
    /// Connect to displayd, create a surface, allocate a backbuf frame token.
    ///
    /// Emits `COMP_FAILSTOP_OK` and returns `Err` if displayd is unavailable,
    /// so the compositor does not hang waiting for a service that isn't running.
    pub fn init() -> Result<Self> {
        let displayd_ep = match registry::lookup_service("displayd:main") {
            Some(ep) => ep,
            None => {
                let _ = debug_print("COMP_FAILSTOP_OK displayd:main not found");
                return Err(Error::NotFound);
            }
        };
        let _ = debug_print("compositor: displayd ep resolved");

        let mut info_msg = Message::new(DISPLAY_OUTPUT_INFO_LABEL, [0; 6], 0);
        if ipc::call(displayd_ep, &mut info_msg, IpcFlags::empty()).is_err() {
            let _ = debug_print("COMP_FAILSTOP_OK displayd OUTPUT_INFO call failed");
            return Err(Error::NotFound);
        }
        let width_px = info_msg.words[0] as u32;
        let height_px = info_msg.words[1] as u32;
        let pitch = info_msg.words[2] as u32;
        if width_px == 0 || height_px == 0 || pitch == 0 {
            let _ = debug_print("COMP_FAILSTOP_OK displayd bad output info");
            return Err(Error::InvalidArgument);
        }
        let _ = debug_print(&alloc::format!(
            "compositor: displayd output {}x{} pitch={}", width_px, height_px, pitch
        ));

        let compositor_space_token = libcluu::boot::space_token();
        let pixel_count = (width_px * height_px) as usize;
        let mut create_msg = Message::new(
            DISPLAY_SURFACE_CREATE_LABEL,
            [
                compositor_space_token,
                COMP_FB_GRANT_VA,
                width_px as usize,
                height_px as usize,
                pitch as usize,
                0,
            ],
            5,
        );
        if ipc::call(displayd_ep, &mut create_msg, IpcFlags::empty()).is_err() {
            let _ = debug_print("COMP_FAILSTOP_OK displayd SURFACE_CREATE call failed");
            return Err(Error::NotFound);
        }
        let surface_token = create_msg.words[0] as u64;
        if surface_token == 0 {
            let _ = debug_print("COMP_FAILSTOP_OK displayd SURFACE_CREATE rejected");
            return Err(Error::InvalidArgument);
        }
        let fb_grant_va = create_msg.words[1];
        let mut fb_backing: Option<Vec<u32>> = None;
        let fb_ptr = if fb_grant_va != 0 {
            let _ = debug_print(&alloc::format!(
                "compositor: direct FB grant at VA={:#x}", fb_grant_va
            ));
            fb_grant_va as *mut u32
        } else {
            let mut v: Vec<u32> = alloc::vec![0u32; pixel_count];
            let p = v.as_mut_ptr();
            fb_backing = Some(v);
            p
        };
        let _ = debug_print("compositor: surface created");

        let geo_msg = Message::new(
            DISPLAY_SET_GEOMETRY_LABEL,
            [0, surface_token as usize, 0, 0, 0, 0],
            4,
        );
        let geo_payload = [0u8, 0u8, 0u8, 0u8, 1u8];
        let _ = ipc::send_msg_with_payload(displayd_ep, &geo_msg, &geo_payload);

        let cols = (width_px / GLYPH_W) as u16;
        let rows = (height_px / GLYPH_H) as u16;
        let cell_count = cols as usize * rows as usize;

        let _ = debug_print(&alloc::format!(
            "compositor: displayd surface {} ({}x{} pitch={})",
            surface_token, width_px, height_px, pitch
        ));

        Ok(Self {
            displayd_ep,
            surface_token,
            width_px,
            height_px,
            pitch,
            fb_ptr,
            fb_grant_va,
            fb_backing,
            cols,
            rows,
            cell_grid: alloc::vec![0u64; cell_count],
            prev_cell_grid: alloc::vec![u64::MAX; cell_count],
            cell_dirty: DirtyCells::new(cols, rows),
            render_dirty: DirtyCells::new(cols, rows),
            palette: xterm_256_palette(),
            dirty_rect: None,
            windows: Vec::new(),
            focused: None,
            active: true,
            next_id: 1,
            clock_seconds: 0,
            clock_ready: false,
            last_clock_now_ms: 0,
            last_flush_at: 0,
            last_flush_tsc: 0,
            tsc_freq_hz: libcluu::syscall::clock_frequency(libcluu::boot::clock_token_handle())
                .unwrap_or(3_000_000_000),
            deadlines: Deadlines::new(),
            instance_id: 0,
            client_endpoint: 0,
            input_endpoint_global: 0,
            control_endpoint: 0,
            registry_endpoint: 0,
            tracked_sessions: BTreeSet::new(),
            pointer_x: 0,
            pointer_y: 0,
            pointer_buttons: 0,
            drag_state: None,
            cursor_needs_render: false,
            pixel_dirty: false,
        })
    }

    /// Poll displayd for output dimensions. If changed, resize the cell grid,
    /// backbuf, and schedule a full redraw.
    pub fn check_output_resize(&mut self) -> bool {
        let mut info_msg = Message::new(DISPLAY_OUTPUT_INFO_LABEL, [0; 6], 0);
        if ipc::call(self.displayd_ep, &mut info_msg, IpcFlags::empty()).is_err() {
            return false;
        }
        let new_w = info_msg.words[0] as u32;
        let new_h = info_msg.words[1] as u32;
        if new_w == 0 || new_h == 0 || (new_w == self.width_px && new_h == self.height_px) {
            return false;
        }

        let _ = debug_print(&alloc::format!(
            "compositor: output resized {}x{} → {}x{}",
            self.width_px, self.height_px, new_w, new_h
        ));

        let new_cols = (new_w / GLYPH_W) as u16;
        let new_rows = (new_h / GLYPH_H) as u16;
        let new_cell_count = new_cols as usize * new_rows as usize;
        let new_pixel_count = (new_w * new_h) as usize;

        // Re-request direct-FB grant for the new dimensions.  The
        // SURFACE_CREATE message layout MUST match init(): [space_token,
        // grant_va, width, height, pitch, 0].  The old grant mapping is
        // overwritten page-by-page when the driver re-grants at the same
        // VA range.
        let compositor_space_token = libcluu::boot::space_token();
        let new_pitch = new_w * 4;
        let mut create_msg = Message::new(
            DISPLAY_SURFACE_CREATE_LABEL,
            [
                compositor_space_token,
                COMP_FB_GRANT_VA,
                new_w as usize,
                new_h as usize,
                new_pitch as usize,
                0,
            ],
            5,
        );
        if ipc::call(self.displayd_ep, &mut create_msg, IpcFlags::empty()).is_err() {
            return false;
        }
        let new_token = create_msg.words[0] as u64;
        if new_token == 0 {
            return false;
        }
        let new_grant_va = create_msg.words[1];
        let mut new_fb_backing = None;
        let new_fb_ptr = if new_grant_va != 0 {
            new_grant_va as *mut u32
        } else {
            let mut backing = alloc::vec![0u32; new_pixel_count];
            let ptr = backing.as_mut_ptr();
            new_fb_backing = Some(backing);
            ptr
        };

        self.width_px = new_w;
        self.height_px = new_h;
        self.cols = new_cols;
        self.rows = new_rows;
        self.cell_grid = alloc::vec![0u64; new_cell_count];
        self.prev_cell_grid = alloc::vec![u64::MAX; new_cell_count];
        self.dirty_rect = None;
        self.cell_dirty.reset(new_cols, new_rows);
        self.render_dirty.reset(new_cols, new_rows);
        self.cell_dirty.mark_all();
        self.surface_token = new_token;
        self.fb_grant_va = new_grant_va;
        self.fb_ptr = new_fb_ptr;
        self.fb_backing = new_fb_backing;

        let geo_msg = Message::new(
            DISPLAY_SET_GEOMETRY_LABEL,
            [0, new_token as usize, 0, 0, 0, 0],
            4,
        );
        let geo_payload = [0u8, 0u8, 0u8, 0u8, 1u8];
        let _ = ipc::send_msg_with_payload(self.displayd_ep, &geo_msg, &geo_payload);

        self.schedule_frame(
            libcluu::syscall::clock_now(libcluu::boot::clock_token_handle())
                .unwrap_or(0),
        );
        true
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

#[cfg(test)]
mod tests {
    use super::DirtyCells;

    #[test]
    fn dirty_cells_deduplicate_and_pop_only_marked_cells() {
        // Given: a 3×2 grid with one cell marked repeatedly.
        let mut dirty = DirtyCells::new(3, 2);
        dirty.push((1, 1));
        dirty.push((1, 1));
        dirty.push((2, 0));

        // When: the renderer drains its targeted cells.
        let first = dirty.pop();
        let second = dirty.pop();
        let third = dirty.pop();

        // Then: each marked cell appears once and unrelated cells never appear.
        assert_eq!(first, Some((2, 0)));
        assert_eq!(second, Some((1, 1)));
        assert_eq!(third, None);
    }

    #[test]
    fn dirty_cells_mark_all_after_resize() {
        // Given: a dirty queue reset to a resized 2×2 cell grid.
        let mut dirty = DirtyCells::new(1, 1);
        dirty.reset(2, 2);

        // When: the resize path requests a complete redraw.
        dirty.mark_all();

        // Then: every cell is queued exactly once.
        assert_eq!(dirty.pop(), Some((1, 1)));
        assert_eq!(dirty.pop(), Some((0, 1)));
        assert_eq!(dirty.pop(), Some((1, 0)));
        assert_eq!(dirty.pop(), Some((0, 0)));
        assert_eq!(dirty.pop(), None);
    }
}
