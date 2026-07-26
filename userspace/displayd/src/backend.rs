//! Backend abstraction — scanout buffer, flush, and direct-scanout trait.
//!
//! The `Backend` trait abstracts over linear-framebuffer and virtio-gpu
//! backends. The pure composition core writes pixels into the scanout
//! buffer via `scanout_buffer_mut` and calls `flush` with the frame
//! damage. Hardware ordering (DMA barriers, cache coherency) remains
//! backend-owned — the core only performs nonvolatile slice copies.
//!
//! `try_direct_scanout` is an opportunistic optimization: if a single
//! surface covers the full output at the right format/pitch, the backend
//! can bypass composition and scanout directly. Returns `true` if applied.

use alloc::vec;
use alloc::vec::Vec;
use cluu_wire::display::{DamageList, OutputInfo};

use crate::surface::Surface;

/// Abstracts the scanout target. Implementors: linear-fb (T7), virtio-gpu.
pub trait Backend {
    /// Output dimensions, pitch, and pixel format.
    fn output_info(&self) -> OutputInfo;

    /// Mutable access to the scanout buffer in XRGB8888 row-major layout.
    /// The slice has `pitch/4 * height` u32 words; each row starts at
    /// `row * pitch/4`. The core writes composed pixels here.
    fn scanout_buffer_mut(&mut self) -> &mut [u32];

    /// Flush `damage` to the actual hardware. Called after composition
    /// completes. The backend translates scene damage to scanout damage
    /// (may be identity if the scanout maps 1:1 to the output).
    fn flush(&mut self, damage: &DamageList);

    /// Opportunistic: if `surface` can be directly scanned out (covers
    /// full output, right format/pitch), bypass composition. Returns
    /// `true` if direct scanout was applied.
    fn try_direct_scanout(&mut self, surface: &Surface) -> bool;
}

/// In-memory backend for host tests. A simple `Vec<u32>` implementing
/// `Backend`, so tests can verify pixel output against golden arrays.
pub struct MemoryBackend {
    info: OutputInfo,
    buffer: Vec<u32>,
}

impl MemoryBackend {
    /// Create a memory backend with the given dimensions and pitch.
    /// `pitch` is bytes per scanline (must be >= width * 4).
    pub fn new(width: u32, height: u32, pitch: u32) -> Self {
        let words_per_row = (pitch / 4) as usize;
        let buf_len = words_per_row * height as usize;
        MemoryBackend {
            info: OutputInfo {
                width,
                height,
                pitch,
                format: cluu_wire::display::PixelFormat::Xrgb8888,
            },
            buffer: vec![0u32; buf_len],
        }
    }

    /// Read a pixel at (x, y) — for test verification.
    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        let pitch_words = self.info.pitch as usize / 4;
        self.buffer[y as usize * pitch_words + x as usize]
    }

    /// Full buffer slice — for golden-array comparison.
    pub fn buffer(&self) -> &[u32] {
        &self.buffer
    }

    /// Fill the entire buffer with a value (test setup).
    pub fn fill(&mut self, value: u32) {
        for px in &mut self.buffer {
            *px = value;
        }
    }
}

impl Backend for MemoryBackend {
    fn output_info(&self) -> OutputInfo {
        self.info
    }

    fn scanout_buffer_mut(&mut self) -> &mut [u32] {
        &mut self.buffer
    }

    fn flush(&mut self, _damage: &DamageList) {
        // No-op: the memory backend's buffer IS the scanout. No hardware
        // barrier needed.
    }

    fn try_direct_scanout(&mut self, _surface: &Surface) -> bool {
        // Memory backend never does direct scanout — always composite.
        false
    }
}
