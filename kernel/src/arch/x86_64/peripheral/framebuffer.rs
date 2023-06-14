use core::{ptr::{addr_of, write_bytes}, slice};

pub struct FrameBuffer {
    pub screen: &'static mut [u32], 
    pub scanline: u32, 
    pub width: u32, 
    pub height: u32
}

impl FrameBuffer {
    pub fn new(screen: *mut u32, scanline: u32, width: u32, height: u32) -> Self {
        Self {
            screen: unsafe {
                let size = (scanline * height) as usize; //get the size of the framebuffer
                write_bytes(screen, 0, size); //init self.screen
                slice::from_raw_parts_mut(screen, size) 
            }, 
            scanline, width, height } }


    pub fn draw_screen_test(&mut self) {
        let s = self.scanline;
        let w = self.width;
        let h = self.height;
    
        if s > 0 {
            // Cross-hair to see self.screen dimension detected correctly
            for y in 0..h {
                self.screen[((s * y + w * 2) >> 2) as usize] = 0x00FFFFFF;
            }
            for x in 0..w {
                self.screen[(((s * (h >> 1) + x * 4)) >> 2) as usize] = 0x00FFFFFF;
            }

            
        }


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
        use bootboot::*;
    
        // Get the font structure pointer
        let font: *mut psf2_t = unsafe { addr_of!(_binary_font_psf_start) } as *const u64 as *mut psf2_t;
        let psf = unsafe { *font };
    
        // Extract font properties
        let headersize = psf.headersize;          // Size of the font header
        let numglyph = psf.numglyph;              // Number of glyphs in the font
        let bytesperglyph = psf.bytesperglyph;    // Size of each glyph in bytes
        let height = psf.height;                   // Height of each glyph
        let width = psf.width;                     // Width of each glyph
        let bpl = (width + 7) / 8;                 // Bytes per line (scanline) of each glyph
        let fb_scanline = unsafe { bootboot.fb_scanline };  // Scanline length of the framebuffer
    
        let mut kx = 0;  // Current horizontal position of the glyph
    
        // Calculate the starting address of the glyph data
        let glyph_start_addr = (font as u64 + headersize as u64) as *mut u8;
    
        // Iterate over each character in the string
        for s in string.bytes() {
            // Calculate the offset of the glyph in the font data
            let glyph_offset = (s as u32).min(numglyph - 1) * bytesperglyph;
    
            // Get a pointer to the glyph data
            let mut glyph = unsafe { glyph_start_addr.offset(glyph_offset as isize) };
    
            // Calculate the starting offset in the framebuffer
            let mut offs = kx * (width + 1) * 4;
    
            // Iterate over each line of the glyph
            for _ in 0..height {
                let mut line = offs as u64;  // Current line offset in the framebuffer
                let mut mask = 1 << (width - 1);  // Bit mask to check each pixel of the glyph
    
                // Iterate over each pixel in the line
                for _ in 0..width {
                    let target_pixel = &mut self.screen[(line / 4) as usize];  // Get a mutable reference to the target pixel in the framebuffer
                    let pixel_value = if unsafe { *glyph } & mask > 0 { 0xFFFFFF } else { 0 };  // Determine the pixel color based on the glyph data
                    *target_pixel = pixel_value;  // Update the pixel value in the framebuffer
                    mask >>= 1;  // Shift the mask to check the next pixel
                    line += 4;  // Move to the next pixel in the line
                }
    
                self.screen[(line / 4) as usize] = 0;  // Set the last pixel in the line to 0 (end of line)
                glyph = unsafe { glyph.offset(bpl as isize) };  // Move to the next line of the glyph data
                offs += fb_scanline;  // Move to the corresponding line in the framebuffer
            }
    
            kx += 1;  // Move to the next horizontal position for the next glyph
        }
    }
}
