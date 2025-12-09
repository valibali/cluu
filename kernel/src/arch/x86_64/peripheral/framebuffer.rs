/*
 * Framebuffer Graphics Driver
 *
 * This module implements a basic framebuffer driver for graphics output.
 * It provides pixel-level access to the display and text rendering
 * capabilities using a PSF2 font.
 *
 * Why this is important:
 * - Enables visual output from the kernel
 * - Provides debugging capabilities through on-screen text
 * - Implements basic graphics primitives for kernel UI
 * - Supports early boot diagnostics and status display
 * - Forms the foundation for any future graphical interfaces
 *
 * The framebuffer is memory-mapped and allows direct pixel manipulation.
 * Text rendering is implemented using bitmap fonts embedded in the kernel.
 */

use core::{
    ptr::{addr_of, write_bytes},
    slice,
};

pub struct FrameBuffer {
    pub screen: &'static mut [u32],
    pub scanline: u32,
    pub width: u32,
    pub height: u32,
}

impl FrameBuffer {
    pub fn new(
        screen: *mut u32,
        scanline: u32,
        width: u32,
        height: u32,
    ) -> Result<FrameBuffer, &'static str> {
        //TODO: Initialization error logic, now just emit Result
        Ok(FrameBuffer {
            screen: unsafe {
                let size = (scanline * height) as usize; //get the size of the framebuffer
                write_bytes(screen, 0, size); //init self.screen
                slice::from_raw_parts_mut(screen, size)
            },
            scanline,
            width,
            height,
        })
        .map_err(|_: &'static str| "Error with Framebuffer mapping!")
    }

    pub fn draw_screen_test(&mut self) {
        let s = self.scanline;
        let w = self.width;
        let h = self.height;

        if s > 0 {
            // Cross-hair to see self.screen dimension detected correctly
            for y in 0..h {
                self.put_pixel(w / 2, y, 0x00FFFFFF)
            }
            for x in 0..w {
                //self.screen[((s * (h >> 1) + x * 4) >> 2) as usize] = 0x00FFFFFF;
                self.put_pixel(x, h / 2, 0x00FFFFFF)
            }
        }

        log::info!("Screentest was drawn.");
    }

    /// Puts a pixel of the specified color at the given coordinates (x, y) on the screen.
    ///
    /// # Arguments
    ///
    /// * `x` - The x-coordinate of the pixel.
    /// * `y` - The y-coordinate of the pixel.
    /// * `color` - The color value of the pixel.
    ///
    /// # Safety
    ///
    /// This function assumes that the pixel coordinates are within the screen dimensions and
    /// that the framebuffer is properly initialized.
    #[inline]
    fn put_pixel(&mut self, x: u32, y: u32, color: u32) {
        // Write the color value to the framebuffer
        *unsafe {
            self.screen
                .get_unchecked_mut(((self.height - 1 - y) * self.scanline / 4 + x) as usize)
        } = color;
    }

    /// Display text on the self.screen using the PC self.screen Font.
    ///
    /// # Arguments
    ///
    /// * `string` - The string to be displayed on the self.screen.
    ///
    /// # Example
    ///
    /// ```rust
    /// let mut self.screen = self.screen::new();
    /// self.screen.puts("Hello, World!");
    /// ```
    pub fn puts(&mut self, string: &'static str) {
        use crate::bootboot::*;

        // Get the font structure pointer
        let font: *mut Psf2T = { addr_of!(_binary_font_psf_start) } as *const u64 as *mut Psf2T;
        let psf = unsafe { *font };

        // Extract font properties
        let headersize = psf.headersize; // Size of the font header
        let numglyph = psf.numglyph; // Number of glyphs in the font
        let bytesperglyph = psf.bytesperglyph; // Size of each glyph in bytes
        let height = psf.height; // Height of each glyph
        let width = psf.width; // Width of each glyph
        let bpl = (width + 7) / 8; // Bytes per line (scanline) of each glyph
        let fb_scanline = unsafe { bootboot.fb_scanline }; // Scanline length of the framebuffer

        // Calculate the starting address of the glyph data
        let glyph_start_addr = (font as u64 + headersize as u64) as *mut u8;

        // Iterate over each character in the string
        for (kx, s) in string.bytes().enumerate() {
            // Calculate the offset of the glyph in the font data
            let glyph_offset = (s as u32).min(numglyph - 1) * bytesperglyph;

            // Get a pointer to the glyph data
            let mut glyph = unsafe { glyph_start_addr.offset(glyph_offset as isize) };

            // Calculate the starting offset in the framebuffer
            let mut offs = kx as u32 * (width + 1) * 4;

            // Iterate over each line of the glyph
            for _ in 0..height {
                let mut line = offs as u64; // Current line offset in the framebuffer
                let mut mask = 1 << (width - 1); // Bit mask to check each pixel of the glyph

                // Iterate over each pixel in the line
                for _ in 0..width {
                    let target_pixel = &mut self.screen[(line / 4) as usize]; // Get a mutable reference to the target pixel in the framebuffer
                    let pixel_value = if unsafe { *glyph } & mask > 0 {
                        0xFFFFFF
                    } else {
                        0
                    }; // Determine the pixel color based on the glyph data
                    *target_pixel = pixel_value; // Update the pixel value in the framebuffer
                    mask >>= 1; // Shift the mask to check the next pixel
                    line += 4; // Move to the next pixel in the line
                }

                self.screen[(line / 4) as usize] = 0; // Set the last pixel in the line to 0 (end of line)
                glyph = unsafe { glyph.offset(bpl as isize) }; // Move to the next line of the glyph data
                offs += fb_scanline; // Move to the corresponding line in the framebuffer
            }
        }
    }
}
