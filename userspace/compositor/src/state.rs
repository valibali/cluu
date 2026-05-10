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
