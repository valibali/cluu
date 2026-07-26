//! Linear framebuffer backend — maps /dev/fb0 WC and flushes damage rects.
//!
//! The `LinearFbBackend` owns the WC-mapped framebuffer pointer and an
//! internal composition buffer. The Scene composites into the composition
//! buffer (via `scanout_buffer_mut`), then `flush` copies damaged rects
//! from the composition buffer to the real framebuffer using
//! `copy_nonoverlapping` — the WC PAT mode coalesces stores into burst
//! writes.
//!
//! `try_direct_scanout` returns `false`: linear FB has no direct-scanout
//! path (all pixels go through composition).

use alloc::vec;
use alloc::vec::Vec;

use cluu_wire::display::{DamageList, OutputInfo, PixelFormat, Rect};

use displayd::backend::Backend;
use displayd::surface::Surface;

use libcluu::posix::{_close, _open, _read, c_void, mmap, O_RDWR, MAP_SHARED, PROT_READ, PROT_WRITE};

/// 40-byte geometry header at the start of /dev/fb0.
const FB_HEADER_MAGIC: u32 = 0x4642_4630; // "FB0\0"
const FB_HEADER_LEN: usize = 40;

/// Raw framebuffer mapping info returned by `map_framebuffer`.
pub struct FramebufferMapping {
    pub ptr: *mut u32,
    #[allow(dead_code)]
    pub phys: u64,
    #[allow(dead_code)]
    pub size: usize,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
}

/// Open /dev/fb0, read the 40-byte geometry header, mmap WC, close fd.
///
/// Mirrors the console/compositor pattern: libcluu's mmap detects the FB
/// magic and routes to `MAP_DEVICE_WC` automatically for `MAP_SHARED` +
/// /dev/fb0 fds.
pub fn map_framebuffer() -> Result<FramebufferMapping, &'static str> {
    let path = b"/dev/fb0\0";
    let fd = _open(path.as_ptr() as *const i8, O_RDWR, 0);
    if fd < 0 {
        return Err("displayd: open /dev/fb0 failed");
    }

    let mut hdr = [0u8; FB_HEADER_LEN];
    let n = _read(fd, hdr.as_mut_ptr() as *mut c_void, FB_HEADER_LEN);
    if n != FB_HEADER_LEN as isize {
        _close(fd);
        return Err("displayd: short read /dev/fb0");
    }

    let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    if magic != FB_HEADER_MAGIC {
        _close(fd);
        return Err("displayd: bad fb header magic");
    }

    let width = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
    let height = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
    let pitch = u32::from_le_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]);
    let fb_size = u64::from_le_bytes([
        hdr[24], hdr[25], hdr[26], hdr[27],
        hdr[28], hdr[29], hdr[30], hdr[31],
    ]) as usize;
    let fb_phys = u64::from_le_bytes([
        hdr[32], hdr[33], hdr[34], hdr[35],
        hdr[36], hdr[37], hdr[38], hdr[39],
    ]);

    let mapped = mmap(
        core::ptr::null_mut::<c_void>(),
        fb_size,
        PROT_READ | PROT_WRITE,
        MAP_SHARED,
        fd,
        0,
    );
    _close(fd);

    if mapped as isize == -1 || mapped.is_null() {
        return Err("displayd: mmap /dev/fb0 failed");
    }

    Ok(FramebufferMapping {
        ptr: mapped as *mut u32,
        phys: fb_phys,
        size: fb_size,
        width,
        height,
        pitch,
    })
}

/// Linear framebuffer backend: owns FB pointer + composition buffer.
///
/// The composition buffer is a `Vec<u32>` with the same pitch/height as
/// the real framebuffer. `scanout_buffer_mut` returns the composition
/// buffer; `flush` copies damaged rects to the real FB.
pub struct LinearFbBackend {
    info: OutputInfo,
    fb_ptr: *mut u32,
    fb_len: usize,
    compose_buffer: Vec<u32>,
}

impl LinearFbBackend {
    pub fn new(fb: FramebufferMapping) -> Self {
        let pitch_words = fb.pitch as usize / 4;
        let buf_len = pitch_words * fb.height as usize;
        LinearFbBackend {
            info: OutputInfo {
                width: fb.width,
                height: fb.height,
                pitch: fb.pitch,
                format: PixelFormat::Xrgb8888,
            },
            fb_ptr: fb.ptr,
            fb_len: buf_len,
            compose_buffer: vec![0u32; buf_len],
        }
    }

    /// Read a pixel from the composition buffer (for self-test verification).
    #[allow(dead_code)]
    pub fn compose_pixel(&self, x: u32, y: u32) -> u32 {
        let pitch_words = self.info.pitch as usize / 4;
        self.compose_buffer[y as usize * pitch_words + x as usize]
    }

    /// Flush a single rect from the composition buffer to the real FB.
    /// Uses `copy_nonoverlapping` — WC PAT mode coalesces stores.
    fn flush_rect(&self, rect: Rect) {
        let pitch_words = self.info.pitch as usize / 4;
        let x = rect.x as usize;
        let y0 = rect.y as usize;
        let copy_w = rect.w as usize;
        let rows = rect.h as usize;

        for row in 0..rows {
            let y = y0 + row;
            if y >= self.info.height as usize {
                break;
            }
            let off = y * pitch_words + x;
            if off + copy_w > self.fb_len {
                break;
            }
            if off + copy_w > self.compose_buffer.len() {
                break;
            }
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.compose_buffer.as_ptr().add(off),
                    self.fb_ptr.add(off),
                    copy_w,
                );
            }
        }
    }
}

impl Backend for LinearFbBackend {
    fn output_info(&self) -> OutputInfo {
        self.info
    }

    fn scanout_buffer_mut(&mut self) -> &mut [u32] {
        &mut self.compose_buffer
    }

    fn flush(&mut self, damage: &DamageList) {
        for r in damage.rects() {
            // Clip to output bounds.
            let bounds = Rect { x: 0, y: 0, w: self.info.width, h: self.info.height };
            if let Some(clipped) = r.clip_to(bounds) {
                self.flush_rect(clipped);
            }
        }
    }

    fn try_direct_scanout(&mut self, _surface: &Surface) -> bool {
        // Linear FB has no direct-scanout path — always composite.
        false
    }
}
