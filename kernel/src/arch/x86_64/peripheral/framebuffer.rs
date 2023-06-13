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
                let size = (scanline * height) as usize;
                write_bytes(screen, 0, size); //init screen
                slice::from_raw_parts_mut(screen, size) 
            }, 
            scanline, width, height } }

    /**************************
     * Display text on screen *
     **************************/
     pub fn puts(&mut self, string: &'static str) {
        use bootboot::*;
    
        let font: *mut psf2_t = unsafe { addr_of!(_binary_font_psf_start) } as *const u64 as *mut psf2_t;
        let psf = unsafe { *font };
        let headersize = psf.headersize;
        let numglyph = psf.numglyph;
        let bytesperglyph = psf.bytesperglyph;
        let height = psf.height;
        let width = psf.width;
        let bpl = (width + 7) / 8;
        let fb_scanline = unsafe { bootboot.fb_scanline };
    
        let mut kx = 0;
        let glyph_start_addr = (font as u64 + headersize as u64) as *mut u8;
    
        for s in string.bytes() {
            let glyph_offset = (s as u32).min(numglyph - 1) * bytesperglyph;
            let mut glyph = unsafe { glyph_start_addr.offset(glyph_offset as isize) };
            let mut offs = kx * (width + 1) * 4;
    
            for _ in 0..height {
                let mut line = offs as u64;
                let mut mask = 1 << (width - 1);
    
                for _ in 0..width {
                    let target_pixel = &mut self.screen[(line / 4) as usize];
                    let pixel_value = if unsafe { *glyph } & mask > 0 { 0xFFFFFF } else { 0 };
                    *target_pixel = pixel_value;
                    mask >>= 1;
                    line += 4;
                }
    
                self.screen[(line / 4) as usize] = 0;
                glyph = unsafe { glyph.offset(bpl as isize) };
                offs += fb_scanline;
            }
    
            kx += 1;
        }
    }
}
