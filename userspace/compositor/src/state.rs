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

    pub windows: Vec<Window>,
    pub focused: Option<WindowId>,
    pub active: bool,
    pub next_id: u64,

    pub clock_seconds: u64,

    // Registry + IPC endpoints — filled in by main after registry::init().
    pub instance_id: u64,
    pub client_endpoint: usize,
    pub input_endpoint_global: usize,
    pub control_endpoint: usize,
    pub registry_endpoint: usize,
}

use libcluu::boot::{
    process_info, PARAM_FB_BASE, PARAM_FB_HEIGHT, PARAM_FB_PHYS,
    PARAM_FB_PITCH, PARAM_FB_SIZE, PARAM_FB_WIDTH,
};
use libcluu::Result;

pub const GLYPH_W: u32 = 8;
pub const GLYPH_H: u32 = 16;

impl Compositor {
    /// Construct from boot params. Allocates cell_grid + backbuf eagerly.
    /// Does not yet open `/dev/fb0` for mmap — that runs after the registry
    /// is ready (see `Compositor::open_fb` below, called from `main` once
    /// the registry endpoint is known).
    pub fn init() -> Result<Self> {
        let info = process_info();
        let fb_ptr = info.params[PARAM_FB_BASE] as *mut u8;
        let fb_phys = info.params[PARAM_FB_PHYS];
        let fb_size = info.params[PARAM_FB_SIZE] as usize;
        let width_px = info.params[PARAM_FB_WIDTH] as u32;
        let height_px = info.params[PARAM_FB_HEIGHT] as u32;
        let pitch = info.params[PARAM_FB_PITCH] as u32;

        let cols = (width_px / GLYPH_W) as u16;
        let rows = (height_px / GLYPH_H) as u16;

        let cell_count = cols as usize * rows as usize;
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
            windows: Vec::new(),
            focused: None,
            active: false,
            next_id: 1,
            clock_seconds: 0,
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
    /// `reply_endpoint` is where keystrokes will be forwarded.
    pub fn handle_win_register(
        &mut self,
        owner_pid: u32,
        req_w: u32,
        req_h: u32,
        title: &str,
        reply_endpoint: usize,
    ) -> Result<(WindowId, u64, u32, u32)> {
        let granted_w = (req_w as u16).min(self.cols);
        let granted_h = (req_h as u16).min(self.rows.saturating_sub(1));
        if granted_w < 5 || granted_h < 5 {
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
            input_endpoint: reply_endpoint,
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
