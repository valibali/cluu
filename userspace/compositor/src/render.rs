//! Render pipeline: glyph blit, backbuf-to-displayd commit, frame timing, and font helpers.
//!
//! All methods are `impl Compositor` blocks; the type itself lives in `state`.

use crate::config::MIN_FRAME_MS;
use crate::state::{Compositor, PixelRect};
use cluu_wire::display::{DISPLAY_BUFFER_COMMIT_LABEL, Rect};
use libcluu::ipc;
use libcluu::types::Message;

impl Compositor {
    /// Blit pixel regions from SHM into the backbuffer.
    ///
    /// For each window that has a pixel region, copy the ARGB32 pixel data
    /// from the shared SHM buffer into the backbuffer at the correct screen
    /// pixel coordinates. Extends `dirty_rect` to cover the blitted area.
    ///
    /// Called from `tick_frame` before `flush_grid_to_backbuf` so that
    /// pixel content overwrites the BG_CELL placeholder that the compose
    /// pass wrote for pixel-region cells.
    pub fn flush_pixel_regions_to_backbuf(&mut self) {
        #[cfg(feature = "bench")]
        let _bench_start = read_tsc();
        #[cfg(feature = "bench")]
        let mut bench_bytes: usize = 0;

        let pitch_words = self.width_px as usize;
        let cols = self.cols as usize;
        let glyph_w = crate::state::GLYPH_W as usize;
        let glyph_h = crate::state::GLYPH_H as usize;

        for win in self.windows.iter() {
            let Some(ref pr) = win.pixel_region else { continue; };

            let base_cell_x = win.x.saturating_add(pr.cell_x) as usize;
            let base_cell_y = win.y.saturating_add(pr.cell_y) as usize;
            let screen_x = base_cell_x * glyph_w;
            let screen_y = base_cell_y * glyph_h;

            if screen_x >= self.width_px as usize || screen_y >= self.height_px as usize {
                continue;
            }

            let pixels_ptr = pr.mapping.as_ptr() as *const u32;
            let pw = pr.pixel_w as usize;
            let ph = pr.pixel_h as usize;

            let max_rows = (self.height_px as usize - screen_y).min(ph);
            let max_cols = (self.width_px as usize - screen_x).min(pw);

            if pr.cell_w == 0 || pr.cell_h == 0 { continue; }

            for row in 0..max_rows {
                let dst_off = (screen_y + row) * pitch_words + screen_x;
                let src_off = row * pw;
                // SAFETY: `pixels_ptr` is the SHM mapping base (NonNull,
                // validated by `ShmMapping::new`). `src_off = row * pw`
                // with `row < max_rows <= ph`, so `src_off + max_cols <=
                // ph * pw` — within the source region. `dst_off` is
                // bounded by `screen_y + max_rows <= height_px` and
                // `screen_x + max_cols <= width_px` (both computed via
                // `.min()` above), so the destination is within
                // `backbuf`. Source (SHM) and dest (backbuf Vec) are
                // disjoint allocations. Both are u32-aligned (SHM is
                // page-aligned; Vec<u32> is u32-aligned).
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        pixels_ptr.add(src_off),
                        self.fb_ptr.add(dst_off),
                        max_cols,
                    );
                }
            }

            #[cfg(feature = "bench")]
            {
                bench_bytes += max_rows * max_cols * 4;
            }

            let _ = cols;
            let pr_rect = PixelRect {
                x: screen_x as u32,
                y: screen_y as u32,
                w: max_cols as u32,
                h: max_rows as u32,
            };
            self.dirty_rect = Some(match self.dirty_rect {
                Some(prev) => prev.extend(pr_rect),
                None => pr_rect,
            });
        }

        #[cfg(feature = "bench")]
        {
            let elapsed = read_tsc().saturating_sub(_bench_start);
            let _ = libcluu::debug_print(&alloc::format!(
                "BENCH_COMP_SHM2BB: cycles={} bytes={}",
                elapsed, bench_bytes
            ));
        }
    }

    /// Flush dirty cells to the framebuffer if active.
    ///
    /// Push-mode: `now_ms` (= cached `last_clock_now_ms`) only advances on
    /// TIME_TICK arrivals (1 Hz). Using it as a deadline gates rendering to
    /// 1 fps, which clobbers per-keystroke echo. Instead: flush on demand
    /// whenever there's dirty work. Frame rate naturally throttled by
    /// WIN_DAMAGE arrival rate.
    ///
    /// Returns `true` if a flush happened.
    pub fn tick_frame(&mut self, now_ms: u64) -> bool {
        if !self.active {
            self.deadlines.next_frame_ms = u64::MAX;
            return false;
        }
        // No windows = nothing to composite. Skip the initial zero flush
        // that would send 2M black pixels to displayd before any content
        // exists. (Bug A: stale-pixel noise at first interior row.)
        if self.windows.is_empty() {
            self.deadlines.next_frame_ms = u64::MAX;
            return false;
        }
        let cells_changed = !self.cell_dirty.is_empty() || self.prev_cell_grid != self.cell_grid;
        if !cells_changed && !self.pixel_dirty {
            self.deadlines.next_frame_ms = u64::MAX;
            return false;
        }
        // Throttle using raw TSC (monotonic, cheap rdtsc) instead of
        // last_clock_now_ms which only advances on 1 Hz TIME_TICK.
        let now_tsc = read_tsc();
        let elapsed_ms = if self.tsc_freq_hz > 0 {
            now_tsc.saturating_sub(self.last_flush_tsc) / (self.tsc_freq_hz / 1000)
        } else {
            MIN_FRAME_MS + 1
        };
        if elapsed_ms < MIN_FRAME_MS {
            self.deadlines.next_frame_ms = self.last_clock_now_ms.saturating_add(MIN_FRAME_MS);
            return false;
        }
        let was_pixel_only = self.pixel_dirty && !cells_changed;
        self.pixel_dirty = false;

        #[cfg(feature = "bench")]
        let _bench_frame_start = read_tsc();

        self.flush_pixel_regions_to_backbuf();
        if !was_pixel_only {
            self.flush_grid_to_backbuf();
        }
        self.flush_backbuf_to_displayd();
        self.deadlines.next_frame_ms = u64::MAX;
        self.last_flush_at = now_ms;
        self.last_flush_tsc = read_tsc();

        #[cfg(feature = "bench")]
        {
            let frame_cycles = read_tsc().saturating_sub(_bench_frame_start);
            // Emit every frame — serial bandwidth is sufficient at TUI rates.
            let _ = libcluu::debug_print(&alloc::format!(
                "BENCH_COMP_FRAME: cycles={}", frame_cycles
            ));
        }

        // One-shot benchmark: after 100 flushes report cycles-per-frame on
        // COM2.  The compositor is single-threaded so these static muts are
        // safe (no concurrent access).
        #[allow(static_mut_refs)]
        // SAFETY: The compositor is single-threaded — the main loop is
        // `ipc_recv_any` → `handle_message` → `tick_frame`, with no
        // `spawn` or extra threads. `FRAME_COUNT` and `FRAME_START_TSC`
        // are only accessed here, so there is no data race. The
        // `static_mut_refs` lint is allowed because this is a known
        // single-threaded context.
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
            // Respect the tick_frame throttle: when a flush happened less
            // than MIN_FRAME_MS ago, arm to the throttle boundary instead of
            // now+MIN_FRAME_MS so the loop wakes the moment flushing is
            // allowed again (lower drag latency).
            let throttle_until = self.last_flush_at.saturating_add(MIN_FRAME_MS);
            self.deadlines.next_frame_ms = if throttle_until > now_ms {
                throttle_until
            } else {
                now_ms.saturating_add(MIN_FRAME_MS)
            };
        }
    }
}

impl Compositor {
    /// Glyph-blit cells whose value differs from `prev_cell_grid` and write
    /// the resulting pixels into `backbuf`. Caller must follow with
    /// `flush_backbuf_to_displayd` to push to displayd.
    pub fn flush_grid_to_backbuf(&mut self) {
        #[cfg(feature = "bench")]
        let _bench_start = read_tsc();
        #[cfg(feature = "bench")]
        let mut bench_cells: usize = 0;

        // BENCH: Bug A diagnostics — identify the first interior row of the
        // most recently registered window so we can trace render/skip
        // decisions for exactly the row that exhibits stale pixel noise.
        #[cfg(feature = "bench")]
        let bench_trace_row: Option<(u16, u16, u16)> = {
            self.windows.iter().max_by_key(|w| w.id).map(|w| {
                let (y_start, x_off) = if w.modal || w.fullscreen || w.no_chrome {
                    (w.y, 0u16)
                } else {
                    // CHROME_TOP=1 + PAD_TOP=0; CHROME_LEFT=1 + PAD_LEFT=1
                    (w.y + 1, 2u16)
                };
                (y_start, w.x + x_off, w.x + w.w)
            })
        };
        #[cfg(feature = "bench")]
        let mut bench_trace_count: usize = 0;

        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let pitch_words = self.width_px as usize; // contiguous backbuf
        let glyph_w = libcluu::atlas::GLYPH_W;
        let glyph_h = libcluu::atlas::GLYPH_H;

        for cy in 0..rows {
            for cx in 0..cols {
                let idx = cy * cols + cx;
                let cell = self.cell_grid[idx];

                // BENCH: log render/skip for the first interior row of the
                // newest window — limits to 20 cells to avoid serial flood.
                #[cfg(feature = "bench")]
                if let Some((trace_y, trace_x0, trace_x1)) = bench_trace_row {
                    if cy == trace_y as usize
                        && cx >= trace_x0 as usize
                        && cx < trace_x1 as usize
                        && bench_trace_count < 20
                    {
                        bench_trace_count += 1;
                        let prev = self.prev_cell_grid[idx];
                        let _ = libcluu::debug_print(&alloc::format!(
                            "BENCH_COMP_GRID_CELL: cx={} cy={} prev={:#018x} cell={:#018x} {}",
                            cx, cy, prev, cell,
                            if prev == cell { "SKIP" } else { "RENDER" },
                        ));
                    }
                }

                if self.prev_cell_grid[idx] == cell {
                    continue;
                }
                self.prev_cell_grid[idx] = cell;

                #[cfg(feature = "bench")]
                {
                    bench_cells += 1;
                }

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

                if cell == crate::compose::PIXEL_CELL {
                    continue;
                }

                let cp = (cell & 0x1F_FFFF) as u32;
                let fg_idx = ((cell >> 21) & 0xFF) as u8;
                let bg_idx = ((cell >> 29) & 0xFF) as u8;
                let attrs = ((cell >> 37) & 0x0F) as u8;
                let bold = (attrs & 0b0001) != 0;
                let underline = (attrs & 0b0010) != 0;
                let reverse = (attrs & 0b0100) != 0;
                let italic = (attrs & 0b1000) != 0;
                let mut fg = self.palette[fg_idx as usize];
                let mut bg = self.palette[bg_idx as usize];
                if reverse {
                    core::mem::swap(&mut fg, &mut bg);
                }

                let px = cx * glyph_w;
                let py = cy * glyph_h;

                if cp == 0x2588 {
                    for row in 0..glyph_h {
                        let off = (py + row) * pitch_words + px;
                        unsafe {
                            for x in 0..glyph_w {
                                *self.fb_ptr.add(off + x) = fg;
                            }
                        }
                    }
                    continue;
                }

                let glyph = match libcluu::font::glyph_alpha_for_codepoint(cp, bold, italic) {
                    Some(g) => g,
                    None => {
                        let ch = unicode_to_cp437(cp);
                        font_glyph_alpha(ch, bold, italic)
                    }
                };

                let mut row_buffer = [0u32; 8];
                for row in 0..glyph_h {
                    let alpha_row = &glyph[row * glyph_w..(row + 1) * glyph_w];
                    libcluu::simd::blend_alpha_row(alpha_row, fg, bg, &mut row_buffer);
                    let off = (py + row) * pitch_words + px;
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            row_buffer.as_ptr(),
                            self.fb_ptr.add(off),
                            glyph_w,
                        );
                    }
                }
                if underline {
                    let off = (py + glyph_h - 1) * pitch_words + px;
                    for x in 0..glyph_w {
                        unsafe { *self.fb_ptr.add(off + x) = fg; }
                    }
                }
            }
        }

        #[cfg(feature = "bench")]
        {
            let elapsed = read_tsc().saturating_sub(_bench_start);
            let _ = libcluu::debug_print(&alloc::format!(
                "BENCH_COMP_GRID2BB: cycles={} cells={}",
                elapsed, bench_cells
            ));
        }
    }

    /// Push the dirty pixel rect from the backbuf to displayd via a shared
    /// frame token.
    ///
    /// Copies the dirty region from `backbuf` into the pre-mapped
    /// inactive backbuf frame, then sends `DISPLAY_BUFFER_COMMIT_LABEL` with
    /// that frame's token and damage rects.  displayd maps the frame, copies
    /// pixels into its surface buffer, and composites+flushes to the real
    /// framebuffer.
    ///
    /// Consumes and clears `dirty_rect` via `.take()`.  If `dirty_rect` is
    /// `None` (nothing changed since the last flush) the function returns
    /// immediately without sending anything to displayd.
    pub fn flush_backbuf_to_displayd(&mut self) {
        let Some(rect) = self.dirty_rect.take() else { return; };

        let x = rect.x.min(self.width_px);
        let y = rect.y.min(self.height_px);
        let right = (rect.x + rect.w).min(self.width_px);
        let bot = (rect.y + rect.h).min(self.height_px);
        if right <= x || bot <= y { return; }
        let w = right - x;
        let h = bot - y;

        let dr = Rect { x, y, w, h };
        let mut damage_bytes = [0u8; 16];
        damage_bytes[0..4].copy_from_slice(&dr.x.to_le_bytes());
        damage_bytes[4..8].copy_from_slice(&dr.y.to_le_bytes());
        damage_bytes[8..12].copy_from_slice(&dr.w.to_le_bytes());
        damage_bytes[12..16].copy_from_slice(&dr.h.to_le_bytes());
        let commit_msg = Message::new(
            DISPLAY_BUFFER_COMMIT_LABEL,
            [
                damage_bytes.len(),
                self.surface_token as usize,
                0,
                0,
                0,
                0,
            ],
            2,
        );
        let _ = ipc::send_msg_with_payload(
            self.displayd_ep,
            &commit_msg,
            &damage_bytes,
        );
    }
}

// Local helpers (file-private).

/// Read the CPU's time-stamp counter. Used only for the bench one-shot.
#[inline]
fn read_tsc() -> u64 {
    // SAFETY: `_rdtsc` is a x86_64 intrinsic that reads the timestamp
    // counter register. It has no memory safety requirements — it cannot
    // fault, requires no valid pointers, and touches no memory. The
    // `unsafe` is required because it's an inline asm intrinsic.
    unsafe { core::arch::x86_64::_rdtsc() }
}
fn font_glyph_alpha(ch: u8, bold: bool, italic: bool) -> [u8; 128] {
    if italic {
        libcluu::font::glyph_alpha_for_cp437_italic(ch)
    } else if bold {
        libcluu::font::glyph_alpha_for_cp437_bold(ch)
    } else {
        libcluu::font::glyph_alpha_for_cp437(ch)
    }
}

fn unicode_to_cp437(cp: u32) -> u8 {
    match cp {
        0x0000..=0x007F => cp as u8,
        0x2500 => 0xC4,                 // ─
        0x2502 => 0xB3,                 // │
        0x250C => 0xDA,                 // ┌
        0x2510 => 0xBF,                 // ┐
        0x2514 => 0xC0,                 // └
        0x2518 => 0xD9,                 // ┘
        0x251C => 0xC3,                 // ├
        0x2524 => 0xB4,                 // ┤
        0x2534 => 0xC1,                 // ┴
        0x252C => 0xC2,                 // ┬
        0x253C => 0xC5,                 // ┼
        0x2550 => 0xCD,                 // ═
        0x2551 => 0xBA,                 // ║
        0x2554 => 0xC9,                 // ╔
        0x2557 => 0xBB,                 // ╗
        0x255A => 0xC8,                 // ╚
        0x255D => 0xBC,                 // ╝
        0x2580 => 0xDF,                 // ▀ upper half
        0x2584 => 0xDC,                 // ▄ lower half
        0x2588 => 0xDB,                 // █ full block
        0x258C => 0xDD,                 // ▌ left half
        0x2590 => 0xDE,                 // ▐ right half
        0x2591 => 0xB0,                 // ░ light shade
        0x2592 => 0xB1,                 // ▒ medium shade
        0x2593 => 0xB2,                 // ▓ dark shade
        0x2581 => 0x01,                 // ▁ eighth block 1/8 (hand glyph)
        0x2582 => 0x02,                 // ▂ eighth block 2/8 (hand glyph)
        0x2583 => 0x03,                 // ▃ eighth block 3/8 (hand glyph)
        0x2585 => 0x04,                 // ▅ eighth block 5/8 (hand glyph)
        0x2586 => 0x05,                 // ▆ eighth block 6/8 (hand glyph)
        0x2587 => 0x06,                 // ▇ eighth block 7/8 (hand glyph)
        0x25B6 => 0x10,                 // ▶ play (CP437 ► hand glyph)
        0x25A0 => 0xFE,                 // ■ stop (CP437 ■ hand glyph)
        _ => b'?',
    }
}
