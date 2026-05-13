//! Render pipeline: glyph blit, backbuf-to-fb flush, frame timing, and font helpers.
//!
//! All methods are `impl Compositor` blocks; the type itself lives in `state`.

use crate::config::{CLOCK_PERIOD_MS, MIN_FRAME_MS};
use crate::state::{Compositor, PixelRect};

impl Compositor {
    /// Tick the frame deadline. If active + dirty cells pending and the
    /// deadline has passed, flush + broadcast + reset the deadline.
    /// Returns `true` if a flush happened (caller should broadcast).
    pub fn tick_frame(&mut self, now_ms: u64) -> bool {
        if !self.active {
            // Park the frame deadline so the event loop blocks on recv
            // instead of tight-spinning at next_timeout_ms == 0 while VT4
            // is hidden. handle_vt_activate re-arms via schedule_frame
            // after repaint_all dirties cells again.
            self.deadlines.next_frame_ms = u64::MAX;
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

        // One-shot benchmark: after 100 flushes report cycles-per-frame on
        // COM2.  The compositor is single-threaded so these static muts are
        // safe (no concurrent access).
        #[allow(static_mut_refs)]
        unsafe {
            static mut FRAME_COUNT: u32 = 0;
            static mut FRAME_START_TSC: u64 = 0;
            FRAME_COUNT += 1;
            if FRAME_COUNT == 1 {
                FRAME_START_TSC = read_tsc();
            } else if FRAME_COUNT == 101 {
                let end = read_tsc();
                let cycles_per_frame = (end - FRAME_START_TSC) / 100;
                let _ = libcluu::debug_print(&alloc::format!(
                    "BENCH_COMP_BLIT: cycles_per_frame={}",
                    cycles_per_frame
                ));
            }
        }

        true
    }

    /// Update `clock_seconds` and dirty row 0 so the status bar re-blits.
    ///
    /// Push-mode: this is called from the TIME_TICK recv arm whenever
    /// timeserver delivers a tick. No internal rate-limit guard — each
    /// invocation corresponds to one logical second.
    pub fn tick_clock(&mut self, now_ms: u64, now_secs: u64) {
        if now_ms > 0 {
            self.clock_ready = true;
        }
        self.clock_seconds = now_secs;
        for cx in 0..self.cols {
            self.cell_dirty.push((cx, 0));
        }
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

// Local helpers (file-private).

/// Read the CPU's time-stamp counter. Used only for the bench one-shot.
#[inline]
fn read_tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}
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
