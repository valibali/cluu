/*
 * Framebuffer Text Console
 *
 * This module provides a text console implementation on top of the framebuffer.
 * It handles character rendering, cursor management, scrolling, and ANSI colors.
 */

use crate::drivers::display::FB;
use crate::bootboot::*;
use core::ptr::addr_of;
use spin::Mutex;

pub static CONSOLE: Mutex<Console> = Mutex::new(Console::new());

#[derive(Clone, Copy)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0 };
    pub const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
    };
    pub const RED: Color = Color { r: 255, g: 0, b: 0 };
    pub const GREEN: Color = Color { r: 0, g: 255, b: 0 };
    pub const BLUE: Color = Color { r: 0, g: 0, b: 255 };
    pub const YELLOW: Color = Color {
        r: 255,
        g: 255,
        b: 0,
    };
    pub const MAGENTA: Color = Color {
        r: 255,
        g: 0,
        b: 255,
    };
    pub const CYAN: Color = Color {
        r: 0,
        g: 255,
        b: 255,
    };
    pub const GRAY: Color = Color {
        r: 128,
        g: 128,
        b: 128,
    };
    pub const LIGHT_GRAY: Color = Color {
        r: 192,
        g: 192,
        b: 192,
    };

    pub fn to_u32(&self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

pub struct Console {
    cursor_x: u32,
    cursor_y: u32,
    char_width: u32,
    char_height: u32,
    cols: u32,
    rows: u32,
    fg_color: Color,
    bg_color: Color,
}

impl Console {
    const fn new() -> Self {
        Self {
            cursor_x: 0,
            cursor_y: 0,
            char_width: 8,   // PSF2 font is typically 8 pixels wide
            char_height: 16, // PSF2 font is typically 16 pixels tall
            cols: 0,
            rows: 0,
            fg_color: Color::WHITE,
            bg_color: Color::BLACK,
        }
    }

    pub fn init(&mut self) {
        log::info!("Console init: Acquiring framebuffer lock...");
        if let Some(ref framebuffer) = *FB.lock() {
            log::info!(
                "Console init: Framebuffer found, dimensions: {}x{}",
                framebuffer.width,
                framebuffer.height
            );
            self.cols = framebuffer.width / self.char_width;
            self.rows = framebuffer.height / self.char_height;
            log::info!("Console init: Character grid: {}x{}", self.cols, self.rows);
            self.cursor_x = 0;
            self.cursor_y = 0;
            log::info!("Console init: Setup complete, skipping clear for now");
        } else {
            log::error!("Console init: No framebuffer available!");
        }
    }

    pub fn clear_screen(&mut self) {
        log::info!("Console clear_screen: Starting...");
        if let Some(ref mut framebuffer) = *FB.lock() {
            log::info!("Console clear_screen: Got framebuffer {}x{}", framebuffer.width, framebuffer.height);
            
            // Use memset-like approach for better performance
            let bg_color = self.bg_color.to_u32();
            let total_pixels = (framebuffer.width * framebuffer.height) as usize;

            log::info!("Console clear_screen: Clearing {} pixels with color 0x{:x}", total_pixels, bg_color);

            // Clear screen efficiently without yielding to avoid deadlock
            for i in 0..total_pixels.min(framebuffer.screen.len()) {
                framebuffer.screen[i] = bg_color;
            }
            
            log::info!("Console clear_screen: Screen cleared");
        } else {
            log::error!("Console clear_screen: No framebuffer available!");
        }
        self.cursor_x = 0;
        self.cursor_y = 0;
        log::info!("Console clear_screen: Complete");
    }

    pub fn set_colors(&mut self, fg: Color, bg: Color) {
        self.fg_color = fg;
        self.bg_color = bg;
    }

    pub fn write_char(&mut self, ch: char) {
        match ch {
            '\n' => {
                self.cursor_x = 0;
                self.cursor_y += 1;
                if self.cursor_y >= self.rows {
                    self.scroll_up();
                }
            }
            '\r' => {
                self.cursor_x = 0;
            }
            '\t' => {
                // Tab = 4 spaces
                for _ in 0..4 {
                    self.write_char(' ');
                }
            }
            ch if ch.is_ascii() && !ch.is_control() => {
                self.draw_char(ch);
                self.cursor_x += 1;
                if self.cursor_x >= self.cols {
                    self.cursor_x = 0;
                    self.cursor_y += 1;
                    if self.cursor_y >= self.rows {
                        self.scroll_up();
                    }
                }
            }
            _ => {
                // Ignore non-printable characters
            }
        }
    }

    pub fn write_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.write_char(ch);
        }
    }

    pub fn write_colored(&mut self, s: &str, fg: Color, bg: Color) {
        let old_fg = self.fg_color;
        let old_bg = self.bg_color;
        self.set_colors(fg, bg);
        self.write_str(s);
        self.set_colors(old_fg, old_bg);
    }

    fn draw_char(&mut self, ch: char) {
        if let Some(ref mut framebuffer) = *FB.lock() {
            let font: *mut Psf2T = { addr_of!(_binary_font_psf_start) } as *const u64 as *mut Psf2T;
            let psf = unsafe { *font };

            let char_code = ch as u32;
            if char_code >= psf.numglyph {
                return; // Character not in font
            }

            let glyph_offset = psf.headersize + char_code * psf.bytesperglyph;
            let glyph_data = unsafe { (font as *const u8).add(glyph_offset as usize) };

            let pixel_x = self.cursor_x * self.char_width;
            let pixel_y = self.cursor_y * self.char_height;

            let fg_color = self.fg_color.to_u32();
            let bg_color = self.bg_color.to_u32();

            // Render the glyph
            for row in 0..self.char_height {
                if row >= psf.height {
                    break;
                }

                let byte_index = (row * ((psf.width + 7) / 8)) as usize;
                let glyph_byte = unsafe { *glyph_data.add(byte_index) };

                for col in 0..self.char_width {
                    if col >= psf.width {
                        break;
                    }

                    let bit = (glyph_byte >> (7 - col)) & 1;
                    let color = if bit == 1 { fg_color } else { bg_color };

                    let x = pixel_x + col;
                    let y = pixel_y + row;

                    if x < framebuffer.width && y < framebuffer.height {
                        framebuffer.put_pixel(x, y, color);
                    }
                }
            }
        }
    }

    fn scroll_up(&mut self) {
        if let Some(ref mut framebuffer) = *FB.lock() {
            let line_height = self.char_height;
            let total_height = framebuffer.height;
            let scanline = framebuffer.scanline / 4; // Convert bytes to u32 pixels

            // Calculate how many rows of pixels to move
            let rows_to_copy = total_height - line_height;

            // Use bulk memory copy for much better performance
            // Source: start of line 1 (after first line_height rows)
            // Destination: start of line 0
            let src_offset = (line_height * scanline) as usize;
            let dst_offset = 0;
            let pixels_to_copy = (rows_to_copy * scanline) as usize;

            // Safety check: ensure we don't go out of bounds
            let fb_len = framebuffer.screen.len();
            if src_offset + pixels_to_copy <= fb_len {
                // Bulk copy using ptr::copy (like memmove)
                // This is MUCH faster than pixel-by-pixel nested loops
                unsafe {
                    let src = framebuffer.screen.as_ptr().add(src_offset);
                    let dst = framebuffer.screen.as_mut_ptr().add(dst_offset);
                    core::ptr::copy(src, dst, pixels_to_copy);
                }
            }

            // Clear the bottom line efficiently
            let bg_color = self.bg_color.to_u32();
            let bottom_line_start = (rows_to_copy * scanline) as usize;
            let bottom_line_pixels = (line_height * scanline) as usize;

            if bottom_line_start + bottom_line_pixels <= fb_len {
                for i in 0..bottom_line_pixels {
                    framebuffer.screen[bottom_line_start + i] = bg_color;
                }
            }
        }

        // Move cursor to last line
        self.cursor_y = self.rows - 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor_x > 0 {
            self.cursor_x -= 1;
            // Clear the character at current position
            self.draw_char(' ');
        } else if self.cursor_y > 0 {
            self.cursor_y -= 1;
            self.cursor_x = self.cols - 1;
            self.draw_char(' ');
        }
    }

    pub fn get_cursor_pos(&self) -> (u32, u32) {
        (self.cursor_x, self.cursor_y)
    }

    pub fn set_cursor_pos(&mut self, x: u32, y: u32) {
        self.cursor_x = x.min(self.cols - 1);
        self.cursor_y = y.min(self.rows - 1);
    }
}

// Public API functions
pub fn init() {
    log::info!("Console API init: Acquiring console lock...");
    CONSOLE.lock().init();
    log::info!("Console API init: Complete");
}

pub fn clear_screen() {
    CONSOLE.lock().clear_screen();
}

pub fn write_char(ch: char) {
    CONSOLE.lock().write_char(ch);
}

pub fn write_str(s: &str) {
    CONSOLE.lock().write_str(s);
}

pub fn write_colored(s: &str, fg: Color, bg: Color) {
    CONSOLE.lock().write_colored(s, fg, bg);
}

pub fn backspace() {
    CONSOLE.lock().backspace();
}

// Macro for easy console printing (with heap allocation)
#[macro_export]
macro_rules! console_print {
    ($fmt:expr) => {
        $crate::utils::io::console::write_str($fmt)
    };
    ($fmt:expr, $($arg:tt)*) => {{
        let mut s = alloc::string::String::new();
        use core::fmt::Write;
        let _ = write!(s, $fmt, $($arg)*);
        $crate::utils::io::console::write_str(&s);
    }};
}

#[macro_export]
macro_rules! console_println {
    () => {
        $crate::utils::io::console::write_str("\n")
    };
    ($fmt:expr) => {{
        $crate::utils::io::console::write_str($fmt);
        $crate::utils::io::console::write_str("\n");
    }};
    ($fmt:expr, $($arg:tt)*) => {{
        let mut s = alloc::string::String::new();
        use core::fmt::Write;
        let _ = write!(s, $fmt, $($arg)*);
        $crate::utils::io::console::write_str(&s);
        $crate::utils::io::console::write_str("\n");
    }};
}
