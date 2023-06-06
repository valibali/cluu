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
#![feature(
    pointer_is_aligned,
    panic_info_message,
    raw_ref_op
)]


// Required for -Z build-std flag.
extern crate rlibc;
extern crate x86_64;
extern crate spin;
extern crate bitflags;
extern crate log;
//extern crate alloc;


use core::panic::PanicInfo;
//use alloc::string::String;
use peripherals::*;
use utils::logger;
use x86_64::instructions::*;

#[allow(dead_code)]
#[allow(non_snake_case)]
#[allow(non_camel_case_types)]
mod bootboot;
mod arch;
mod peripherals;
mod syscall;
mod utils;

pub use log::{debug, error, info, set_max_level, warn};


/******************************************
 * Entry point, called by BOOTBOOT Loader *
 ******************************************/
#[no_mangle] // don't mangle the name of this function
fn _start() -> ! {
    /*** NOTE: this code runs on all cores in parallel ***/
    use bootboot::*;

    unsafe {init_noncpu_perif();}
    
    let logger_init_result = logger::init(true); //clearscr: true

    match logger_init_result {
        Ok(_) => info!("Logger initialized correctly"),
        Err(err) => println!("Error with initializing logger: {}", err),
    }

    //Lets use the BOOTBOOT_INFO as a pointer, dereference it and immediately borrow it.
    let bootboot_r = unsafe { & (*(BOOTBOOT_INFO as *const BOOTBOOT)) };
    let fb = BOOTBOOT_FB as u64;

    

    if bootboot_r.fb_scanline > 0 {

        // cross-hair to see screen dimension detected correctly
        for y in 0..bootboot_r.fb_height {
            let addr = fb
                + bootboot_r.fb_scanline as u64 * y as u64
                + bootboot_r.fb_width as u64 * 2;
            unsafe { *(addr as *mut u32) = 0x00FFFFFF };
        }
        for x in 0..bootboot_r.fb_width {
            let addr = fb
                + bootboot_r.fb_scanline as u64 * (bootboot_r.fb_height / 2) as u64 + (x * 4) as u64;
            unsafe { *(addr as *mut u32) = 0x00FFFFFF };
        }

        //ed, green, blue boxes in order
        for y in 0..20 {
            for x in 0..20 {
                let addr = fb
                    + bootboot_r.fb_scanline as u64 * (y + 20) as u64
                    + (x + 20) * 4;
                unsafe { *(addr as *mut u32) = 0x00FF0000 };
            }
        }
        for y in 0..20 {
            for x in 0..20 {
                let addr = fb
                    + bootboot_r.fb_scanline as u64 * (y + 20) as u64
                    + (x + 50) * 4;
                unsafe { *(addr as *mut u32) = 0x0000FF00 };
            }
        }
        for y in 0..20 {
            for x in 0..20 {
                let addr = fb
                    + bootboot_r.fb_scanline as u64 * (y + 20) as u64
                    + (x + 80) * 4;
                unsafe { *(addr as *mut u32) = 0x000000FF };
            }
        }
    }

    puts("Ha latod a crosshair-t akkor a bootloader jol lotte be a GOP felbontast ;)");
    loop {}
}

/**************************
 * Display text on screen *
 **************************/
fn puts(string: &str) {
    use bootboot::*;

    let fb: u64 = BOOTBOOT_FB as u64;
    let bootboot_r: &BOOTBOOT = unsafe { & (*(BOOTBOOT_INFO as *const BOOTBOOT)) };

    unsafe {
        let font: *mut psf2_t = &_binary_font_psf_start as *const u64 as *mut psf2_t;
        let (mut kx, mut line, mut mask, mut offs): (u32, u64, u64, u32);
        kx = 0;
        let bpl: u32 = ((*font).width + 7) / 8;

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
                    let target_location: *mut u32 = (fb as *const u8 as u64 + line) as *mut u32;
                    let mut target_value: u32 = 0;
                    if (*glyph as u64) & (mask) > 0 {
                        target_value = 0xFFFFFF;
                    }
                    *target_location = target_value;
                    mask >>= 1;
                    line += 4;
                }
                let target_location: *mut u32 = (fb as *const u8 as u64 + line) as *mut u32;
                *target_location = 0;
                glyph = glyph.offset(bpl as isize);
                offs += bootboot_r.fb_scanline;
            }
            kx += 1;
        }
    }
}


/*************************************
 * This function is called on panic. *
 *************************************/

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    
    let payload = match _info.payload().downcast_ref::<&str>() {
        Some(s) => *s,
        None => "Panic occured - no further info available",
        // until heap allocator is not available, this stays commented 
        // None => match info.payload().downcast_ref::<String>() {
        //     Some(s) => &s[..],
        //     None => "Box<Any>",
        // },
    };
    println!("{}", payload);

    match _info.message() {
        Some(arg) => println!("Panic message: {}", arg),
        None => println!("No panic message available"),
    };

    match _info.location() {
        Some(loc) => println!("Panic occured in file '{}' at line {}:{}", loc.file(), loc.line(), loc.column()),
        None => println!("No location info available"),    
    };
  
    loop {
        hlt();
    }
    
}
