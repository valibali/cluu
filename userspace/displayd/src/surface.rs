//! Surface ownership and buffer slot management.
//!
//! A `Surface` owns its double-buffered pixel data and tracks its
//! position, z-order, visibility, and display (scaled) dimensions. The
//! buffer lifecycle state machine (`SurfaceState` from `cluu_wire::display`)
//! is used for validation at creation time; the composition core uses a
//! simpler front/back model — the full IPC state machine lives in the
//! displayd service layer (T7).
//!
//! # Buffer model
//!
//! Each surface has `NUM_BUFFERS` (2) `Vec<u32>` buffers. `front` tracks
//! which buffer is currently displayed. `present` swaps the front buffer.
//! The composition reads from the front buffer via `displayed_pixels`.
//!
//! Normal SHM buffers use nonvolatile slice copies (`copy_from_slice`).
//! Hardware ordering (DMA barriers) remains backend-owned.

use alloc::vec;
use alloc::vec::Vec;
use cluu_wire::display::{DamageList, Error, Rect, SurfaceState, NUM_BUFFERS};

/// Server-owned surface with pixel data, geometry, and buffer management.
pub struct Surface {
    pub token: u64,
    /// Content width in pixels.
    pub width: u32,
    /// Content height in pixels.
    pub height: u32,
    /// Content pitch in bytes per scanline (>= width * 4).
    pub pitch: u32,
    /// Double-buffered pixel data. Each buffer is `pitch/4 * height` u32s.
    pub buffer_data: [Vec<u32>; NUM_BUFFERS],
    /// Which buffer (0 or 1) is currently displayed (front buffer).
    pub front: u8,
    /// Scene-space X position (may be negative — partially off-screen).
    pub x: i32,
    /// Scene-space Y position (may be negative).
    pub y: i32,
    /// Z-order for painter's algorithm (lower = farther back).
    pub z_order: i32,
    /// Visibility flag. Hidden surfaces are not composited.
    pub visible: bool,
    /// Displayed width in scene coords. Equals `width` for unscaled,
    /// `width * N` for integer N× scaling.
    pub display_w: u32,
    /// Displayed height in scene coords.
    pub display_h: u32,
    /// True after `destroy` — further operations return `InvalidCapability`.
    pub destroyed: bool,
    /// Last committed content damage (content coords).
    pub last_content_damage: DamageList,
}

impl Surface {
    /// Create a new surface. Uses `SurfaceState::new` for validation
    /// (non-zero dimensions, pitch overflow check).
    pub fn new(token: u64, width: u32, height: u32, pitch: u32) -> Result<Self, Error> {
        // Validate via the wire-level state machine.
        let _ = SurfaceState::new(token, width, height, pitch)?;
        let words_per_row = (pitch / 4) as usize;
        let buf_len = words_per_row * height as usize;
        Ok(Surface {
            token,
            width,
            height,
            pitch,
            buffer_data: [vec![0u32; buf_len], vec![0u32; buf_len]],
            front: 0,
            x: 0,
            y: 0,
            z_order: 0,
            visible: true,
            display_w: width,
            display_h: height,
            destroyed: false,
            last_content_damage: DamageList::empty(),
        })
    }

    /// Write pixel data into a buffer. The slice length must not exceed
    /// the buffer's capacity (`pitch/4 * height`).
    pub fn write_buffer(&mut self, buffer_index: u8, pixels: &[u32]) -> Result<(), Error> {
        let idx = buffer_index as usize;
        if idx >= NUM_BUFFERS {
            return Err(Error::BufferOverflow);
        }
        if self.destroyed {
            return Err(Error::InvalidCapability);
        }
        let buf = &mut self.buffer_data[idx];
        if pixels.len() > buf.len() {
            return Err(Error::InvalidRect);
        }
        buf[..pixels.len()].copy_from_slice(pixels);
        Ok(())
    }

    /// Write a single pixel at (x, y) in the buffer.
    pub fn write_pixel(
        &mut self,
        buffer_index: u8,
        x: u32,
        y: u32,
        pixel: u32,
    ) -> Result<(), Error> {
        let idx = buffer_index as usize;
        if idx >= NUM_BUFFERS {
            return Err(Error::BufferOverflow);
        }
        if self.destroyed {
            return Err(Error::InvalidCapability);
        }
        let pitch_words = self.pitch as usize / 4;
        let off = y as usize * pitch_words + x as usize;
        if off >= self.buffer_data[idx].len() {
            return Err(Error::InvalidRect);
        }
        self.buffer_data[idx][off] = pixel;
        Ok(())
    }

    /// Present a buffer: set it as the front (displayed) buffer and record
    /// the content damage. This is the test/helper API — the real displayd
    /// service (T7) will use the full `SurfaceState` state machine over IPC.
    pub fn present(&mut self, buffer_index: u8, damage: DamageList) -> Result<(), Error> {
        let idx = buffer_index as usize;
        if idx >= NUM_BUFFERS {
            return Err(Error::BufferOverflow);
        }
        if self.destroyed {
            return Err(Error::InvalidCapability);
        }
        self.front = buffer_index;
        self.last_content_damage = damage;
        Ok(())
    }

    /// Get the pixels of the currently displayed (front) buffer.
    pub fn displayed_pixels(&self) -> Option<&[u32]> {
        if self.destroyed {
            return None;
        }
        let idx = self.front as usize;
        if idx < NUM_BUFFERS {
            Some(&self.buffer_data[idx])
        } else {
            None
        }
    }

    /// Pitch in u32 words (bytes / 4).
    pub fn pitch_words(&self) -> usize {
        self.pitch as usize / 4
    }

    /// The scene-space rect clipped to output bounds, or `None` if fully
    /// off-screen or zero-sized.
    pub fn clipped_scene_rect(&self, output_w: u32, output_h: u32) -> Option<Rect> {
        if self.display_w == 0 || self.display_h == 0 {
            return None;
        }
        let x0 = self.x.max(0) as u32;
        let y0 = self.y.max(0) as u32;
        let x1 = self
            .x
            .saturating_add(self.display_w as i32)
            .min(output_w as i32)
            .max(0) as u32;
        let y1 = self
            .y
            .saturating_add(self.display_h as i32)
            .min(output_h as i32)
            .max(0) as u32;
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Destroy the surface. Releases buffer memory. Further operations
    /// return `Error::InvalidCapability`.
    pub fn destroy(&mut self) {
        self.destroyed = true;
        self.buffer_data[0].clear();
        self.buffer_data[1].clear();
    }
}
