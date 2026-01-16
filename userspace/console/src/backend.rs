//! Console backend abstraction.
//!
//! A backend is responsible for writing pixels to the display surface. The
//! console renderer stays unaware of how pixels reach the screen, which makes
//! it possible to swap the framebuffer out for a shared-memory or GPU-backed
//! device later.

/// Backend trait for low-level pixel output.
pub trait ConsoleBackend {
    /// Return the pixel width of the output surface.
    fn width(&self) -> usize;
    /// Return the pixel height of the output surface.
    fn height(&self) -> usize;
    /// Write a single pixel into the output surface.
    fn put_pixel(&mut self, x: usize, y: usize, color: u32);
}

/// Framebuffer-backed console output.
///
/// This backend writes directly into the boot-provided framebuffer.
pub struct FramebufferBackend {
    fb: *mut u8,
    width: usize,
    height: usize,
    pitch: usize,
}

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
}
