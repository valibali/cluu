//! Client-side pixel region helper for compositor windows.
//!
//! Allocates a frame token for an ARGB32 pixel buffer, maps it into the
//! caller's address space, and provides a write API. The frame token is
//! sent to the compositor via `COMP_WIN_SET_PIXEL_REGION_LABEL` so the
//! compositor can map the same pages and blit pixels directly to its
//! backbuffer.
//!
//! Pixel dimensions are `cell_w * GLYPH_W` × `cell_h * GLYPH_H` where
//! GLYPH_W=8, GLYPH_H=16 (the compositor's font cell size).

extern crate alloc;

use crate::boot::space_token;
use crate::syscall::{self, InvokeOp, MAP_FRAME_TOKEN};
use crate::{Error, Result};

pub const GLYPH_W: usize = 8;
pub const GLYPH_H: usize = 16;

const FLAGS_USER_RW: usize = 0x07;
const PAGE_SIZE: usize = 4096;

/// Virtual address where pixel SHM is mapped. Chosen to not collide with
/// the compositor's per-window VA range (0xC100_0000+) or APP_FB_BASE
/// (0xA000_0000).
const PIXEL_SHM_VA: usize = 0xD100_0000;

/// A mapped ARGB32 pixel buffer shared with the compositor.
///
/// Created by [`PixelRegion::alloc`]. The caller writes pixels via
/// [`write_pixel`] or [`write_row`], bumps [`flush`], and sends
/// `COMP_WIN_DAMAGE_LABEL` to the compositor so it blits the region.
pub struct PixelRegion {
    ptr: *mut u32,
    token: u64,
    pub pixel_w: usize,
    pub pixel_h: usize,
    pub cell_w: u16,
    pub cell_h: u16,
}

impl PixelRegion {
    /// Allocate and map a pixel buffer for `cell_w × cell_h` compositor cells.
    ///
    /// The pixel dimensions are `cell_w * 8 × cell_h * 16`. Returns the
    /// region plus the frame token to send to the compositor.
    pub fn alloc(cell_w: u16, cell_h: u16) -> Result<Self> {
        let pixel_w = cell_w as usize * GLYPH_W;
        let pixel_h = cell_h as usize * GLYPH_H;
        let total_pixels = pixel_w * pixel_h;
        let total_bytes = total_pixels * 4;
        let rounded = (total_bytes + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let sp = space_token();
        if sp == 0 {
            return Err(Error::InvalidArgument);
        }

        let token =
            unsafe { syscall::invoke(sp, InvokeOp::FrameAllocate, rounded, 0, 0, 0)? };

        let num_pages = rounded / PAGE_SIZE;
        if let Err(err) = syscall::space_map_range(
            sp,
            PIXEL_SHM_VA,
            token as usize,
            FLAGS_USER_RW | MAP_FRAME_TOKEN,
            num_pages,
            0,
        ) {
            let _ = syscall::space_unmap(sp, PIXEL_SHM_VA, num_pages);
            unsafe {
                let _ = syscall::invoke(
                    token as usize,
                    InvokeOp::FrameFree,
                    0,
                    0,
                    0,
                    0,
                );
            }
            return Err(err);
        }

        // Zero the buffer so the first frame isn't garbage.
        unsafe {
            core::ptr::write_bytes(
                PIXEL_SHM_VA as *mut u8,
                0,
                total_pixels * 4,
            );
        }

        Ok(PixelRegion {
            ptr: PIXEL_SHM_VA as *mut u32,
            token: token as u64,
            pixel_w,
            pixel_h,
            cell_w,
            cell_h,
        })
    }

    /// Frame token to send to the compositor in `COMP_WIN_SET_PIXEL_REGION_LABEL`.
    pub fn frame_token(&self) -> u64 {
        self.token
    }

    /// Write one ARGB32 pixel at `(x, y)` in pixel coordinates.
    ///
    /// No bounds check — caller must ensure `x < pixel_w` and `y < pixel_h`.
    pub fn write_pixel(&mut self, x: usize, y: usize, argb: u32) {
        let off = y * self.pixel_w + x;
        unsafe { core::ptr::write_volatile(self.ptr.add(off), argb) };
    }

    /// Write a row of ARGB32 pixels at pixel row `y`, starting at pixel
    /// column `x`. `row.len()` must not exceed `pixel_w - x`.
    pub fn write_row(&mut self, x: usize, y: usize, row: &[u32]) {
        let off = y * self.pixel_w + x;
        unsafe {
            core::ptr::copy_nonoverlapping(
                row.as_ptr(),
                self.ptr.add(off),
                row.len(),
            );
        }
    }

    /// Write a full ARGB32 pixel buffer. `buf.len()` must equal
    /// `pixel_w * pixel_h`.
    pub fn write_buf(&mut self, buf: &[u32]) {
        unsafe {
            core::ptr::copy_nonoverlapping(
                buf.as_ptr(),
                self.ptr,
                buf.len().min(self.pixel_w * self.pixel_h),
            );
        }
    }

    /// Raw pointer to the pixel buffer (for direct unsafe access).
    pub fn as_ptr(&self) -> *mut u32 {
        self.ptr
    }

    /// Unmap the client-side SHM mapping without freeing the frame token.
    ///
    /// Once a pixel region is attached to a compositor window, the compositor
    /// owns the shared token and frees it when the window is destroyed.
    pub fn unmap(self) {
        let sp = space_token();
        let total_bytes = self.pixel_w * self.pixel_h * 4;
        let num_pages = (total_bytes + PAGE_SIZE - 1) / PAGE_SIZE;
        let _ = syscall::space_unmap(sp, PIXEL_SHM_VA, num_pages);
    }

    /// Free the frame token and unmap the SHM.
    pub fn destroy(self) {
        let token = self.token;
        self.unmap();
        if token != 0 {
            unsafe {
                let _ = syscall::invoke(
                    token as usize,
                    InvokeOp::FrameFree,
                    0,
                    0,
                    0,
                    0,
                );
            }
        }
    }
}
