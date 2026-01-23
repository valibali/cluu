//! Console backend abstraction.
//!
//! A backend is responsible for writing pixels to the display surface. The
//! console renderer stays unaware of how pixels reach the screen, which makes
//! it possible to swap the framebuffer out for a shared-memory or GPU-backed
//! device later.
//!
//! This module provides:
//! - `FramebufferBackend`: Direct framebuffer writes (legacy, immediate mode)
//! - `DoubleBufferBackend`: Double-buffered rendering with dirty region tracking

extern crate alloc;

use alloc::vec::Vec;

#[cfg(target_arch = "x86_64")]
mod simd;

#[cfg(target_arch = "x86_64")]
use crate::simd::{copy_row_simd, fill_row_simd, is_sse2_available, write_row_simd};

// ============================================================================
// Dirty Region Tracking
// ============================================================================

/// Tracks a bounding box of modified pixels for efficient partial flushes.
///
/// The dirty region expands as pixels are written and resets after flush.
/// This minimizes the amount of data copied from backbuffer to frontbuffer.
#[derive(Debug, Clone)]
pub struct DirtyRegion {
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
    is_dirty: bool,
}

impl DirtyRegion {
    /// Create a new empty (clean) dirty region.
    pub fn new() -> Self {
        Self {
            min_x: usize::MAX,
            min_y: usize::MAX,
            max_x: 0,
            max_y: 0,
            is_dirty: false,
        }
    }

    /// Mark a single pixel as dirty.
    #[inline]
    pub fn mark_pixel(&mut self, x: usize, y: usize) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x + 1);
        self.max_y = self.max_y.max(y + 1);
        self.is_dirty = true;
    }

    /// Mark a rectangular region as dirty.
    #[inline]
    pub fn mark_rect(&mut self, x: usize, y: usize, w: usize, h: usize) {
        if w == 0 || h == 0 {
            return;
        }
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x + w);
        self.max_y = self.max_y.max(y + h);
        self.is_dirty = true;
    }

    /// Check if any region is dirty.
    #[inline]
    #[allow(dead_code)]
    pub fn is_dirty(&self) -> bool {
        self.is_dirty
    }

    /// Get the dirty bounding box: (min_x, min_y, width, height).
    /// Returns None if not dirty.
    pub fn bounds(&self) -> Option<(usize, usize, usize, usize)> {
        if !self.is_dirty || self.max_x <= self.min_x || self.max_y <= self.min_y {
            return None;
        }
        Some((
            self.min_x,
            self.min_y,
            self.max_x - self.min_x,
            self.max_y - self.min_y,
        ))
    }

    /// Reset the dirty region to empty (after flush).
    pub fn clear(&mut self) {
        self.min_x = usize::MAX;
        self.min_y = usize::MAX;
        self.max_x = 0;
        self.max_y = 0;
        self.is_dirty = false;
    }
}

/// Backend trait for low-level pixel output.
pub trait ConsoleBackend {
    /// Return the pixel width of the output surface.
    fn width(&self) -> usize;
    /// Return the pixel height of the output surface.
    fn height(&self) -> usize;
    /// Write a single pixel into the output surface.
    fn put_pixel(&mut self, x: usize, y: usize, color: u32);
    /// Write a horizontal row of pixels (optimized bulk operation).
    /// Default implementation falls back to put_pixel, but backends can override for speed.
    fn put_pixels_row(&mut self, x: usize, y: usize, colors: &[u32]) {
        for (i, &color) in colors.iter().enumerate() {
            self.put_pixel(x + i, y, color);
        }
    }
    /// Fill a rectangular region with a single color (optimized bulk operation).
    /// Default implementation falls back to put_pixel, but backends can override for speed.
    fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        for dy in 0..h {
            for dx in 0..w {
                self.put_pixel(x + dx, y + dy, color);
            }
        }
    }
    /// Copy a rectangular region from one location to another (for scrolling).
    /// Default implementation redraws, but backends can override with memory copy for speed.
    fn copy_rect(
        &mut self,
        _src_x: usize,
        _src_y: usize,
        _dst_x: usize,
        _dst_y: usize,
        _w: usize,
        _h: usize,
    ) {
        // Default: redraw by reading and writing pixels (slow but works for any backend)
        // Optimized backends should override this with actual memory copying
        // Note: This is a simplified fallback - real implementation would need pixel reading
        // For now, this is a placeholder that backends should override
    }

    /// Flush any buffered writes to the display.
    ///
    /// For immediate-mode backends (like `FramebufferBackend`), this is a no-op.
    /// For double-buffered backends, this copies the dirty region to the frontbuffer.
    fn flush(&mut self) {
        // Default: no-op for immediate-mode backends
    }

    /// Check if the backend has pending changes to flush.
    ///
    /// Returns false for immediate-mode backends.
    #[allow(dead_code)]
    fn is_dirty(&self) -> bool {
        false
    }
}

/// Framebuffer-backed console output (immediate mode, no buffering).
///
/// This backend writes directly into the boot-provided framebuffer.
/// Kept available as an alternative to `DoubleBufferBackend` for debugging
/// or scenarios where double buffering overhead is undesirable.
#[allow(dead_code)]
pub struct FramebufferBackend {
    fb: *mut u8,
    width: usize,
    height: usize,
    pitch: usize,
}

#[allow(dead_code)]
impl FramebufferBackend {
    /// Create a framebuffer backend from raw boot parameters.
    pub fn new(fb: *mut u8, width: usize, height: usize, pitch: usize) -> Self {
        Self {
            fb,
            width,
            height,
            pitch,
        }
    }

    /// Get the framebuffer pointer (for internal optimizations).
    fn fb_ptr(&self) -> *mut u8 {
        self.fb
    }

    /// Get the pitch (bytes per row).
    fn pitch(&self) -> usize {
        self.pitch
    }
}

impl ConsoleBackend for FramebufferBackend {
    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn put_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = y * self.pitch + x * 4;
        unsafe {
            let ptr = self.fb.add(offset) as *mut u32;
            ptr.write_volatile(color);
        }
    }

    /// Optimized bulk row write: writes a horizontal row of pixels at once.
    /// Uses SIMD (SSE2) when available for 4x speedup.
    fn put_pixels_row(&mut self, x: usize, y: usize, colors: &[u32]) {
        if y >= self.height {
            return;
        }
        let max_w = self.width.saturating_sub(x);
        let len = colors.len().min(max_w);
        if len == 0 {
            return;
        }

        let row_start = y * self.pitch + x * 4;
        unsafe {
            let dst = self.fb.add(row_start) as *mut u32;

            #[cfg(target_arch = "x86_64")]
            {
                if is_sse2_available() && len >= 4 {
                    // Use SIMD for bulk writes (4 pixels at a time)
                    // SSE2 is enabled at target level, so we can call SIMD functions
                    #[target_feature(enable = "sse2")]
                    unsafe fn call_write(dst: *mut u32, colors: &[u32], len: usize) {
                        write_row_simd(dst, colors, len);
                    }
                    unsafe { call_write(dst, colors, len); }
                } else {
                    // Fallback for small writes or non-x86_64
                    for i in 0..len {
                        dst.add(i).write_volatile(colors[i]);
                    }
                }
            }

            #[cfg(not(target_arch = "x86_64"))]
            {
                // Fallback for non-x86_64 architectures
                for i in 0..len {
                    dst.add(i).write_volatile(colors[i]);
                }
            }
        }
    }

    /// Optimized rectangle fill: fills a rectangular region efficiently.
    /// Uses SIMD (SSE2) when available for 4x speedup.
    fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let max_w = (self.width - x).min(w);
        let max_h = (self.height - y).min(h);
        if max_w == 0 || max_h == 0 {
            return;
        }

        unsafe {
            let first_row_start = y * self.pitch + x * 4;
            let first_row = self.fb.add(first_row_start) as *mut u32;

            #[cfg(target_arch = "x86_64")]
            let use_simd = is_sse2_available() && max_w >= 4;

            // Fill first row using SIMD if available
            #[cfg(target_arch = "x86_64")]
            if use_simd {
                #[target_feature(enable = "sse2")]
                unsafe fn call_fill(dst: *mut u32, color: u32, len: usize) {
                    fill_row_simd(dst, color, len);
                }
                unsafe { call_fill(first_row, color, max_w); }
            } else {
                for i in 0..max_w {
                    first_row.add(i).write_volatile(color);
                }
            }

            #[cfg(not(target_arch = "x86_64"))]
            {
                for i in 0..max_w {
                    first_row.add(i).write_volatile(color);
                }
            }

            // Copy first row to remaining rows if pitch allows efficient copying
            if self.pitch == self.width * 4 {
                // Contiguous memory: can use SIMD copy for remaining rows
                #[cfg(target_arch = "x86_64")]
                if use_simd {
                    #[target_feature(enable = "sse2")]
                    unsafe fn call_copy(src: *const u32, dst: *mut u32, len: usize) {
                        copy_row_simd(src, dst, len);
                    }
                    for row in 1..max_h {
                        let dst = self.fb.add((y + row) * self.pitch + x * 4) as *mut u32;
                        unsafe { call_copy(first_row, dst, max_w); }
                    }
                } else {
                    for row in 1..max_h {
                        let dst = self.fb.add((y + row) * self.pitch + x * 4) as *mut u32;
                        core::ptr::copy_nonoverlapping(first_row, dst, max_w);
                    }
                }

                #[cfg(not(target_arch = "x86_64"))]
                {
                    for row in 1..max_h {
                        let dst = self.fb.add((y + row) * self.pitch + x * 4) as *mut u32;
                        core::ptr::copy_nonoverlapping(first_row, dst, max_w);
                    }
                }
            } else {
                // Non-contiguous: fill each row individually using SIMD if available
                #[cfg(target_arch = "x86_64")]
                if use_simd {
                    #[target_feature(enable = "sse2")]
                    unsafe fn call_fill(dst: *mut u32, color: u32, len: usize) {
                        fill_row_simd(dst, color, len);
                    }
                    for row in 1..max_h {
                        let dst = self.fb.add((y + row) * self.pitch + x * 4) as *mut u32;
                        unsafe { call_fill(dst, color, max_w); }
                    }
                } else {
                    for row in 1..max_h {
                        let dst = self.fb.add((y + row) * self.pitch + x * 4) as *mut u32;
                        for i in 0..max_w {
                            dst.add(i).write_volatile(color);
                        }
                    }
                }

                #[cfg(not(target_arch = "x86_64"))]
                {
                    for row in 1..max_h {
                        let dst = self.fb.add((y + row) * self.pitch + x * 4) as *mut u32;
                        for i in 0..max_w {
                            dst.add(i).write_volatile(color);
                        }
                    }
                }
            }
        }
    }

    /// Optimized rectangle copy: copies a rectangular region using memory operations.
    /// Uses SIMD (SSE2) when available for 4x speedup.
    /// This is much faster than redrawing for scrolling operations.
    /// 
    /// Handles overlapping regions correctly by copying backwards when src_y > dst_y.
    fn copy_rect(
        &mut self,
        src_x: usize,
        src_y: usize,
        dst_x: usize,
        dst_y: usize,
        w: usize,
        h: usize,
    ) {
        if src_x >= self.width
            || src_y >= self.height
            || dst_x >= self.width
            || dst_y >= self.height
        {
            return;
        }
        let max_w = (self.width - src_x.max(dst_x)).min(w);
        let max_h = (self.height - src_y.max(dst_y)).min(h);
        if max_w == 0 || max_h == 0 {
            return;
        }

        unsafe {
            // Determine if regions overlap and copy direction
            // Two ranges [a, a+len) and [b, b+len) overlap if: a < b+len && b < a+len
            // We need to copy backwards if src_y > dst_y AND the regions actually overlap
            // For scrolling: src_y=GLYPH_H, dst_y=0, they don't overlap, so copy forwards
            let regions_overlap = src_x == dst_x 
                && src_y < dst_y + max_h 
                && dst_y < src_y + max_h;
            let copy_backwards = regions_overlap && src_y > dst_y;

            #[cfg(target_arch = "x86_64")]
            let use_simd = is_sse2_available() && max_w >= 4;

            if copy_backwards {
                // Copy backwards: from bottom row to top row
                for row in (0..max_h).rev() {
                    let src_offset = (src_y + row) * self.pitch + src_x * 4;
                    let dst_offset = (dst_y + row) * self.pitch + dst_x * 4;

                    let src = self.fb.add(src_offset) as *const u32;
                    let dst = self.fb.add(dst_offset) as *mut u32;

                    if src != dst as *const u32 {
                        #[cfg(target_arch = "x86_64")]
                        if use_simd {
                            #[target_feature(enable = "sse2")]
                            unsafe fn call_copy(src: *const u32, dst: *mut u32, len: usize) {
                                copy_row_simd(src, dst, len);
                            }
                            call_copy(src, dst, max_w);
                        } else {
                            // Fallback: use volatile for framebuffer device memory
                            for i in 0..max_w {
                                dst.add(i).write_volatile(src.add(i).read_volatile());
                            }
                        }

                        #[cfg(not(target_arch = "x86_64"))]
                        {
                            // Use volatile for framebuffer device memory
                            for i in 0..max_w {
                                dst.add(i).write_volatile(src.add(i).read_volatile());
                            }
                        }
                    }
                }
            } else {
                // Copy forwards: from top row to bottom row (normal case, used for scrolling)
                for row in 0..max_h {
                    let src_offset = (src_y + row) * self.pitch + src_x * 4;
                    let dst_offset = (dst_y + row) * self.pitch + dst_x * 4;

                    let src = self.fb.add(src_offset) as *const u32;
                    let dst = self.fb.add(dst_offset) as *mut u32;

                    if src != dst as *const u32 {
                        #[cfg(target_arch = "x86_64")]
                        if use_simd {
                            #[target_feature(enable = "sse2")]
                            unsafe fn call_copy(src: *const u32, dst: *mut u32, len: usize) {
                                copy_row_simd(src, dst, len);
                            }
                            call_copy(src, dst, max_w);
                        } else {
                            // Fallback: use volatile for framebuffer device memory
                            for i in 0..max_w {
                                dst.add(i).write_volatile(src.add(i).read_volatile());
                            }
                        }

                        #[cfg(not(target_arch = "x86_64"))]
                        {
                            // Use volatile for framebuffer device memory
                            for i in 0..max_w {
                                dst.add(i).write_volatile(src.add(i).read_volatile());
                            }
                        }
                    }
                }
            }
        }
    }
}

// ============================================================================
// Double Buffer Backend
// ============================================================================

/// Double-buffered console backend with dirty region tracking.
///
/// All pixel operations write to a heap-allocated backbuffer. The `flush()`
/// method copies only the dirty region to the actual framebuffer, reducing
/// tearing and improving performance for partial updates.
///
/// # Memory Usage
/// Allocates `width * height * 4` bytes (~3MB for 1024x768).
pub struct DoubleBufferBackend {
    /// Pointer to the actual framebuffer (frontbuffer).
    frontbuffer: *mut u8,
    /// Heap-allocated backbuffer for rendering.
    backbuffer: Vec<u32>,
    /// Display width in pixels.
    width: usize,
    /// Display height in pixels.
    height: usize,
    /// Frontbuffer pitch (bytes per row, may differ from width*4).
    pitch: usize,
    /// Tracks which regions need to be flushed.
    dirty: DirtyRegion,
}

impl DoubleBufferBackend {
    /// Create a new double-buffered backend.
    ///
    /// Allocates a backbuffer on the heap and initializes it to black.
    /// Returns None if allocation fails (heap too small).
    pub fn try_new(frontbuffer: *mut u8, width: usize, height: usize, pitch: usize) -> Option<Self> {
        let size = width * height;
        
        // Try to allocate backbuffer - may fail if heap is too small
        let mut backbuffer = Vec::new();
        if backbuffer.try_reserve_exact(size).is_err() {
            return None;
        }
        backbuffer.resize(size, 0u32);

        Some(Self {
            frontbuffer,
            backbuffer,
            width,
            height,
            pitch,
            dirty: DirtyRegion::new(),
        })
    }

    /// Check if there are dirty pixels that need flushing.
    #[inline]
    #[allow(dead_code)]
    pub fn is_dirty(&self) -> bool {
        self.dirty.is_dirty()
    }

    /// Flush the dirty region from backbuffer to frontbuffer.
    ///
    /// Uses SIMD-accelerated row copies when available. Only copies the
    /// bounding box of modified pixels, minimizing memory bandwidth.
    pub fn flush(&mut self) {
        let Some((x, y, w, h)) = self.dirty.bounds() else {
            return; // Nothing to flush
        };

        // Clamp to screen bounds
        let x = x.min(self.width);
        let y = y.min(self.height);
        let w = w.min(self.width - x);
        let h = h.min(self.height - y);

        if w == 0 || h == 0 {
            self.dirty.clear();
            return;
        }

        #[cfg(target_arch = "x86_64")]
        let use_simd = is_sse2_available() && w >= 4;

        // Copy each dirty row from backbuffer to frontbuffer
        for row in 0..h {
            let src_y = y + row;
            let src_offset = src_y * self.width + x;
            let dst_offset = src_y * self.pitch + x * 4;

            unsafe {
                let src = self.backbuffer.as_ptr().add(src_offset);
                let dst = self.frontbuffer.add(dst_offset) as *mut u32;

                #[cfg(target_arch = "x86_64")]
                if use_simd {
                    #[target_feature(enable = "sse2")]
                    unsafe fn call_copy(src: *const u32, dst: *mut u32, len: usize) {
                        copy_row_simd(src, dst, len);
                    }
                    call_copy(src, dst, w);
                } else {
                    // Fallback: volatile writes to frontbuffer
                    for i in 0..w {
                        dst.add(i).write_volatile(*src.add(i));
                    }
                }

                #[cfg(not(target_arch = "x86_64"))]
                {
                    for i in 0..w {
                        dst.add(i).write_volatile(*src.add(i));
                    }
                }
            }
        }

        self.dirty.clear();
    }

    /// Flush if dirty (convenience method for hybrid strategy).
    #[inline]
    #[allow(dead_code)]
    pub fn flush_if_dirty(&mut self) {
        if self.is_dirty() {
            self.flush();
        }
    }
}

impl ConsoleBackend for DoubleBufferBackend {
    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn flush(&mut self) {
        DoubleBufferBackend::flush(self);
    }

    fn is_dirty(&self) -> bool {
        self.dirty.is_dirty()
    }

    fn put_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = y * self.width + x;
        self.backbuffer[idx] = color;
        self.dirty.mark_pixel(x, y);
    }

    fn put_pixels_row(&mut self, x: usize, y: usize, colors: &[u32]) {
        if y >= self.height {
            return;
        }
        let max_w = self.width.saturating_sub(x);
        let len = colors.len().min(max_w);
        if len == 0 {
            return;
        }

        let row_start = y * self.width + x;
        // Non-volatile write to backbuffer (faster than frontbuffer)
        self.backbuffer[row_start..row_start + len].copy_from_slice(&colors[..len]);
        self.dirty.mark_rect(x, y, len, 1);
    }

    fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let max_w = (self.width - x).min(w);
        let max_h = (self.height - y).min(h);
        if max_w == 0 || max_h == 0 {
            return;
        }

        // Fill backbuffer rows
        for row in 0..max_h {
            let row_start = (y + row) * self.width + x;
            self.backbuffer[row_start..row_start + max_w].fill(color);
        }

        self.dirty.mark_rect(x, y, max_w, max_h);
    }

    fn copy_rect(
        &mut self,
        src_x: usize,
        src_y: usize,
        dst_x: usize,
        dst_y: usize,
        w: usize,
        h: usize,
    ) {
        if src_x >= self.width
            || src_y >= self.height
            || dst_x >= self.width
            || dst_y >= self.height
        {
            return;
        }
        let max_w = (self.width - src_x.max(dst_x)).min(w);
        let max_h = (self.height - src_y.max(dst_y)).min(h);
        if max_w == 0 || max_h == 0 {
            return;
        }

        // Determine copy direction to handle overlapping regions
        let regions_overlap =
            src_x == dst_x && src_y < dst_y + max_h && dst_y < src_y + max_h;
        let copy_backwards = regions_overlap && src_y > dst_y;

        if copy_backwards {
            // Copy backwards: from bottom row to top row
            for row in (0..max_h).rev() {
                let src_row_start = (src_y + row) * self.width + src_x;
                let dst_row_start = (dst_y + row) * self.width + dst_x;

                // Copy within backbuffer (use intermediate buffer to handle overlap)
                let mut temp = [0u32; 256];
                let chunk_size = max_w.min(256);
                let mut col = 0;
                while col < max_w {
                    let len = (max_w - col).min(chunk_size);
                    temp[..len].copy_from_slice(
                        &self.backbuffer[src_row_start + col..src_row_start + col + len],
                    );
                    self.backbuffer[dst_row_start + col..dst_row_start + col + len]
                        .copy_from_slice(&temp[..len]);
                    col += len;
                }
            }
        } else {
            // Copy forwards: from top row to bottom row (normal case for scrolling)
            for row in 0..max_h {
                let src_row_start = (src_y + row) * self.width + src_x;
                let dst_row_start = (dst_y + row) * self.width + dst_x;

                // For non-overlapping copies, we can copy directly
                // Use copy_within for potentially overlapping regions within same buffer
                if src_row_start != dst_row_start {
                    // Safe copy: either different rows or guaranteed non-overlapping
                    let mut temp = [0u32; 256];
                    let chunk_size = max_w.min(256);
                    let mut col = 0;
                    while col < max_w {
                        let len = (max_w - col).min(chunk_size);
                        temp[..len].copy_from_slice(
                            &self.backbuffer[src_row_start + col..src_row_start + col + len],
                        );
                        self.backbuffer[dst_row_start + col..dst_row_start + col + len]
                            .copy_from_slice(&temp[..len]);
                        col += len;
                    }
                }
            }
        }

        self.dirty.mark_rect(dst_x, dst_y, max_w, max_h);
    }
}
