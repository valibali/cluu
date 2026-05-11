//! Compositor core state types and init.
//!
//! `Compositor` is the long-lived owner of the framebuffer mapping,
//! cell grid, window list, and IPC token table. `Window` describes one
//! tenant's region + SHM mapping. `WindowShm` is the on-the-wire header
//! that lives at the start of each per-window shared region (32 bytes,
//! cells follow) — canonical definition lives in `libcluu::window_shm`.
//!
//! Window lifecycle, focus management, input forwarding → `window_mgr`
//! Render pipeline, timing                               → `render`

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

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
    /// Create a `Deadlines` with the clock task armed immediately (fires on
    /// first event-loop iteration) and the frame task parked at `u64::MAX`.
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

pub use libcluu::window_shm::{WindowShm, WIN_SHM_MAGIC, WIN_SHM_VERSION};

/// A mapped SHM region shared between the compositor and one window client.
///
/// Wraps the raw `*mut u8` pointer so that all unsafe pointer arithmetic is
/// centralised here. Callers use the safe `header()` and `read_cell()`
/// accessors instead of scattering `unsafe` blocks across the codebase.
pub struct ShmMapping {
    ptr: core::ptr::NonNull<u8>,
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
        let cells_ptr = unsafe { self.ptr.as_ptr().add(header_size) as *const u64 };
        let off = iy as usize * inner_w as usize + ix as usize;
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
}

/// Long-lived compositor state.  Single instance per process, owned by `main`.
///
/// Initialised by `Compositor::init` (opens `/dev/fb0`, sets up buffers and
/// palette).  All other fields are populated by `main` after registry init.
///
/// Methods are split across `window_mgr` (window lifecycle + input) and
/// `render` (glyph blit, flush, frame/clock timing).
pub struct Compositor {
    /// Write-combined mapping of the hardware framebuffer (via `/dev/fb0` mmap).
    pub fb_ptr: *mut u8,
    /// Physical base address of the framebuffer (from /dev/fb0 header, for debug).
    pub fb_phys: u64,
    /// Total byte length of the fb mapping.
    pub fb_size: usize,
    /// Screen dimensions in pixels.
    pub width_px: u32,
    pub height_px: u32,
    /// Framebuffer row pitch in *bytes* (may differ from `width_px * 4`).
    pub pitch: u32,

    /// Screen dimensions in cell units.
    pub cols: u16,
    pub rows: u16,
    /// Current composed cell grid (cols×rows packed u64 cells).
    pub cell_grid: Vec<u64>,
    /// Shadow copy of the last-flushed grid; used to skip unchanged cells.
    pub prev_cell_grid: Vec<u64>,
    /// Cells marked dirty since the last compose pass; drained by `recompute_dirty`.
    pub cell_dirty: Vec<(u16, u16)>,

    /// xterm-256 ARGB palette; index = colour number from cell fg/bg fields.
    pub palette: [u32; 256],
    /// Software backbuffer (width_px×height_px u32 ARGB).  Written by
    /// `flush_grid_to_backbuf`; pushed to `fb_ptr` by `flush_backbuf_to_fb`.
    pub backbuf: Vec<u32>,

    /// Pixel-level bounding box of cells blitted since the last fb flush.
    /// `None` means nothing was redrawn; `flush_backbuf_to_fb` is a no-op.
    pub dirty_rect: Option<PixelRect>,

    /// Window list in z-order; last entry is the topmost (focused) window.
    pub windows: Vec<Window>,
    /// `WindowId` of the currently focused window, or `None` if no windows exist.
    pub focused: Option<WindowId>,
    /// `true` while the compositor's VT is the active VT; `false` when hidden.
    pub active: bool,
    /// Monotonically increasing counter used to assign `WindowId`s.
    pub next_id: u64,

    /// Monotonic seconds from boot, refreshed by `tick_clock` each second.
    pub clock_seconds: u64,

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
}

use libcluu::posix::{
    _close, _open, _read, mmap, c_void, O_RDWR, MAP_SHARED, PROT_READ, PROT_WRITE,
};
use libcluu::{Error, Result};

pub const GLYPH_W: u32 = 8;
pub const GLYPH_H: u32 = 16;

use crate::config::FB_HEADER_MAGIC;

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
