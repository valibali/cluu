/*
 * mykernel/rust/src/main.rs
 *
 * Copyright (C) 2017 - 2022 Vinay Chandra, Valkony Balázs
 *
 * Permission is hereby granted, free of charge, to any person
 * obtaining a copy of this software and associated documentation
 * files (the "Software"), to deal in the Software without
 * restriction, including without limitation the rights to use, copy,
 * modify, merge, publish, distribute, sublicense, and/or sell copies
 * of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be
 * included in all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
 * EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
 * MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
 * NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
 * HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
 * WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
 * DEALINGS IN THE SOFTWARE.
 *
 * This file is part of the BOOTBOOT Protocol package.
 * @brief A sample BOOTBOOT compatible kernel
 *
 */

// configure Rust compiler
#![no_std]
#![no_main]
#![feature(pointer_byte_offsets)]

// Required for -Z build-std flag.
extern crate rlibc;
extern crate x86_64;
extern crate spin;
extern crate bitflags;
extern crate log;
//extern crate alloc;


use core::panic::PanicInfo;
use core::ptr::*;
//use alloc::string::String;
use devices::*;
use utils::logger;
use x86_64::instructions::*;

#[allow(dead_code)]
#[allow(non_snake_case)]
#[allow(non_camel_case_types)]
mod bootboot;
mod arch;
mod devices;
mod syscall;
mod utils;

pub use log::{debug, error, info, set_max_level, warn};


/******************************************
 * Entry point, called by BOOTBOOT Loader *
 ******************************************/
#[no_mangle] // don't mangle the name of this function
fn _start() -> ! {
    
    init_noncpu_perif();
    
    let logger_init_result = logger::init(true); //clearscr: true

    match logger_init_result {
        Ok(_) => info!("Logger initialized correctly"),
        Err(err) => serial_println!("Error with initializing logger: {}", err),
    }


    let s = unsafe { bootboot::bootboot.fb_scanline } as u32;
    let w = unsafe { bootboot::bootboot.fb_width } as u32;
    let h = unsafe { bootboot::bootboot.fb_height } as u32;

    // Extract the framebuffer pointer as a mutable pointer
    let fb = unsafe { addr_of_mut!(bootboot::fb) } as *mut u32;

    if s > 0 {
        // Cross-hair to see screen dimension detected correctly
        for y in 0..h {
            unsafe { write(fb.byte_offset((s * y + w * 2) as isize), 0x00FFFFFF) };
        }
        for x in 0..w {
            unsafe { write(fb.byte_offset((s * (h >> 1) + x * 4) as isize), 0x00FFFFFF) };
        }

        // Red, green, blue boxes in order
        for y in 0..20 { for x in 0..20 { 
            unsafe { write(fb.byte_offset((s * (y + 20) + (x + 20) * 4) as isize), 0x00FF0000) };}}

        for y in 0..20 { for x in 0..20 {
            unsafe { write(fb.byte_offset((s * (y + 20) + (x + 50) * 4) as isize), 0x0000FF00) };}}

        for y in 0..20 { for x in 0..20 {
                unsafe { write(fb.byte_offset((s * (y + 20) + (x + 80) * 4) as isize), 0x000000FF) };}}
    }

    // say hello
    puts("Hello from a simple BOOTBOOT kernel\n");
    
   
    loop {
        hlt();
    }

}

/**************************
 * Display text on screen *
 **************************/
 fn puts(string: &'static str) {
    use bootboot::*;

    let fb_ptr = unsafe { addr_of_mut!(bootboot::fb) } as u64;
    let font: *mut psf2_t = unsafe { addr_of!(_binary_font_psf_start)} as *const u64 as *mut psf2_t;

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
                let target_pixel = (fb_ptr + line) as *mut u32;
                let pixel_value = if unsafe { *glyph } & mask > 0 { 0xFFFFFF } else { 0 };
                unsafe { target_pixel.write(pixel_value) };
                mask >>= 1;
                line += 4;
            }

            let target_pixel = (fb_ptr + line) as *mut u32;
            unsafe { target_pixel.write(0) };
            glyph = unsafe { glyph.offset(bpl as isize) };
            offs += fb_scanline;
        }

        kx += 1;
    }
}





/*************************************
 * This function is called on panic. *
 *************************************/

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    
    serial_println!("Error: {}", _info);
  
    loop {
        hlt();
    }
    
}
