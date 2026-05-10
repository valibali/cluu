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

    /// Push the entire backbuf to the framebuffer mapping. Plain memcpy under
    /// WC; perf-bench in the FB workstream proved this is the fastest path
    /// once writes are write-combined.
    ///
    /// No-op when `fb_ptr` is null (compositor spawned without FB mapping,
    /// e.g. shell-spawn in test harness before T24 wires up procmgr autostart).
    pub fn flush_backbuf_to_fb(&self) {
        if self.fb_ptr.is_null() {
            return;
        }
        let bytes = self.width_px as usize * self.height_px as usize * 4;
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.backbuf.as_ptr() as *const u8,
                self.fb_ptr,
                bytes,
            );
        }
    }
}

// Local helpers (later might be moved to libcluu if compdemo needs them).
fn font_glyph(ch: u8) -> [u8; 16] {
    // T13: empty glyphs — all-bg-coloured cells prove the blit pipeline
    // end-to-end. Proper ASCII + CP437 font integration lands in T14.
    let _ = ch;
    [0u8; 16]
}

fn unicode_to_cp437(cp: u32) -> u8 {
    if cp < 0x80 { cp as u8 } else { b'?' }
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
