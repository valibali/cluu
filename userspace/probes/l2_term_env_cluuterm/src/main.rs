//! Spec 2 §12 acceptance marker: verify TERM=xterm-256color
//!
//! Reads getenv("TERM") and checks it matches "xterm-256color".

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use core::ffi::CStr;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let marker = b"l2_term_env_cluuterm";
    // libcluu::posix re-exports getenv via `pub use env::*`
    let term_ptr = libcluu::posix::getenv(b"TERM\0".as_ptr() as *const i8) as *const u8;
    let term_str = if term_ptr.is_null() {
        ""
    } else {
        unsafe {
            let s = CStr::from_ptr(term_ptr as *const i8);
            match s.to_str() {
                Ok(s) => s,
                Err(_) => "",
            }
        }
    };

    if term_str == "xterm-256color" {
        let _ = libcluu::debug_print(&format!("{}: PASS\n", core::str::from_utf8(marker).unwrap_or("?")));
        0
    } else {
        let _ = libcluu::debug_print(&format!("{}: FAIL (got TERM='{}')\n", core::str::from_utf8(marker).unwrap_or("?"), term_str));
        1
    }
}