/*
 * mykernel/rust/src/main.rs
 *
 * Copyright (C) 2017 - 2021 Vinay Chandra
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

#[cfg(not(test))]
use core::panic::PanicInfo;

#[allow(dead_code)]
#[allow(non_snake_case)]
#[allow(non_camel_case_types)]
mod bootboot;

// Required for -Z build-std flag.
extern crate rlibc;

/******************************************
 * Entry point, called by BOOTBOOT Loader *
 ******************************************/
#[no_mangle] // don't mangle the name of this function
fn _start() -> ! {
    /*** NOTE: this code runs on all cores in parallel ***/
    unsafe {
        if bootboot::bootboot.fb_scanline > 0 {
        
            let fb = &bootboot::fb as *const u8 as u64;

            // cross-hair to see screen dimension detected correctly
            for y in 0..bootboot::bootboot.fb_height {
                let addr = fb
                    + bootboot::bootboot.fb_scanline as u64 * y as u64
                    + bootboot::bootboot.fb_width as u64 * 2;
                *(addr as *mut u64) = 0x00FFFFFF;
            }
            for x in 0..bootboot::bootboot.fb_width {
                let addr = fb
                    + bootboot::bootboot.fb_scanline as u64 * (bootboot::bootboot.fb_height / 2) as u64 + (x * 4) as u64;
                *(addr as *mut u64) = 0x00FFFFFF;
            }

            // red, green, blue boxes in order
            for y in 0..20 {
                for x in 0..20 {
                    let addr = fb
                        + bootboot::bootboot.fb_scanline as u64 * (y + 20) as u64
                        + (x + 20) * 4;
                    *(addr as *mut u64) = 0x00FF0000;
                }
            }
            for y in 0..20 {
                for x in 0..20 {
                    let addr = fb
                        + bootboot::bootboot.fb_scanline as u64 * (y + 20) as u64
                        + (x + 50) * 4;
                    *(addr as *mut u64) = 0x0000FF00;
                }
            }
            for y in 0..20 {
                for x in 0..20 {
                    let addr = fb
                        + bootboot::bootboot.fb_scanline as u64 * (y + 20) as u64
                        + (x + 80) * 4;
                    *(addr as *mut u64) = 0x000000FF;
                }
            }
        }

        // say hello
        puts("Hello from a simple BOOTBOOT kernel");
    }
    // hang for now
    loop {}
}

/**************************
 * Display text on screen *
 **************************/
fn puts(string: &'static str) {
    use bootboot::*;
    unsafe {
        let font: *mut bootboot::psf2_t = &_binary_font_psf_start as *const u64 as *mut psf2_t;
        let (mut kx, mut line, mut mask, mut offs): (u32, u64, u64, u32);
        kx = 0;
        let bpl = ((*font).width + 7) / 8;

        for s in string.bytes() {
            let glyph_a: *mut u8 = (font as u64 + (*font).headersize as u64) as *mut u8;
            let mut glyph: *mut u8 = glyph_a.offset(
                (if s > 0 && (s as u32) < (*font).numglyph {
                    s as u32
                } else {
                    0
                } * ((*font).bytesperglyph)) as isize,
            );
            offs = kx * ((*font).width + 1) * 4;
            for _y in 0..(*font).height {
                line = offs as u64;
                mask = 1 << ((*font).width - 1);
                for _x in 0..(*font).width {
                    let target_location = (&bootboot::fb as *const u8 as u64 + line) as *mut u32;
                    let mut target_value: u32 = 0;
                    if (*glyph as u64) & (mask) > 0 {
                        target_value = 0xFFFFFF;
                    }
                    *target_location = target_value;
                    mask >>= 1;
                    line += 4;
                }
                let target_location = (&bootboot::fb as *const u8 as u64 + line) as *mut u32;
                *target_location = 0;
                glyph = glyph.offset(bpl as isize);
                offs += bootboot.fb_scanline;
            }
            kx += 1;
        }
    }
}

/*************************************
 * This function is called on panic. *
 *************************************/
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
