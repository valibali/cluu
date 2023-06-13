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
#![feature(pointer_byte_offsets, const_mut_refs)]


// Required for -Z build-std flag.
extern crate rlibc;
extern crate x86_64;
extern crate spin;
extern crate bitflags;
extern crate log;
//extern crate alloc;


use core::panic::PanicInfo;
//use alloc::string::String;

use arch::kstart;
use x86_64::instructions::*;

#[allow(dead_code)]
#[allow(non_snake_case)]
#[allow(non_camel_case_types)]
mod bootboot;
mod arch;
mod syscall;
mod utils;

pub use log::{debug, error, info, set_max_level, warn};


/******************************************
 * Entry point, called by BOOTBOOT Loader *
 ******************************************/
#[no_mangle] // don't mangle the name of this function
fn _start() -> ! {
   
    kstart();
    
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
