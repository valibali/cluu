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

    //there is no pointer arithmetic in rust, so extract the framebuffer pointer as an integer
    let fb = unsafe {addr_of_mut!(bootboot::fb)} as u64; 

    if s > 0 {
        // cross-hair to see screen dimension detected correctly
        for y in 0..h { unsafe { write((fb + (s * y + w * 2) as u64) as *mut u32, 0x00FFFFFF) };}
        for x in 0..w { unsafe { write((fb + (s * (h >> 1) + x * 4) as u64) as *mut u32, 0x00FFFFFF) };}

        //red, green, blue boxes in order
        for y in 0..20 {
            for x in 0..20 {
                    unsafe { write((fb + (s * (y + 20) + (x + 20) * 4) as u64) as *mut u32, 0x000000FF) };
            }
        }
        for y in 0..20 {
            for x in 0..20 {
                    unsafe { write((fb + (s * (y + 20) + (x + 50) * 4) as u64) as *mut u32, 0x0000FF00) };
            }
        }
        for y in 0..20 {
            for x in 0..20 {
                    unsafe { write((fb + (s * (y + 20) + (x + 80) * 4) as u64) as *mut u32, 0x00FF0000) };
            }
        }
    }

        // say hello
        //puts("Hello from a simple BOOTBOOT kernel");
    
    for i in 0..20 {
        info!("Test {}", i);
        warn!("Test {}", i);
        debug!("Test {}", i);
        error!("Test {}", i);
    }
    
    panic!("paniic");

    loop {
        hlt();
    }

}

/**************************
 * Display text on screen *
 **************************/
// fn puts(string: &str) {
//     use bootboot::*;
//     unsafe {
//         let font: *mut bootboot::psf2_t = &_binary_font_psf_start as *const u64 as *mut psf2_t;
//         let (mut kx, mut line, mut mask, mut offs): (u32, u64, u64, u32);
//         kx = 0;
//         let bpl = ((*font).width + 7) / 8;

//         for s in string.bytes() {
//             let glyph_a: *mut u8 = (font as u64 + (*font).headersize as u64) as *mut u8;
//             let mut glyph: *mut u8 = glyph_a.offset(
//                 (if s > 0 && (s as u32) < (*font).numglyph {
//                     s as u32
//                 } else {
//                     0
//                 } * ((*font).bytesperglyph)) as isize,
//             );
//             offs = kx * ((*font).width + 1) * 4;
//             for _y in 0..(*font).height {
//                 line = offs as u64;
//                 mask = 1 << ((*font).width - 1);
//                 for _x in 0..(*font).width {
//                     let target_location = (&bootboot::fb as *const u8 as u64 + line) as *mut u32;
//                     let mut target_value: u32 = 0;
//                     if (*glyph as u64) & (mask) > 0 {
//                         target_value = 0xFFFFFF;
//                     }
//                     *target_location = target_value;
//                     mask >>= 1;
//                     line += 4;
//                 }
//                 let target_location = (&bootboot::fb as *const u8 as u64 + line) as *mut u32;
//                 *target_location = 0;
//                 glyph = glyph.offset(bpl as isize);
//                 offs += bootboot.fb_scanline;
//             }
//             kx += 1;
//         }
//     }

// }


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
