//! Spec 2 §12 acceptance marker: verify tcgetattr returns ICANON|ECHO|ISIG
//!
//! Calls tcgetattr(0) (stdin) and checks the lflag bitmask.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let marker = b"l2_tcgetattr_default";
    let mut t: libcluu::posix::termios::Termios = unsafe { core::mem::zeroed() };
    let rc = libcluu::posix::termios::tcgetattr(0, &mut t as *mut _);

    const ICANON: u32 = 0x0002;
    const ECHO: u32 = 0x0004;
    const ISIG: u32 = 0x0001;

    if rc == 0
        && (t.c_lflag & ICANON) != 0
        && (t.c_lflag & ECHO) != 0
        && (t.c_lflag & ISIG) != 0
    {
        let _ = libcluu::debug_print(&format!("{}: PASS\n", core::str::from_utf8(marker).unwrap_or("?")));
        0
    } else {
        let _ = libcluu::debug_print(&format!(
            "{}: FAIL (rc={}, lflag=0x{:x})\n",
            core::str::from_utf8(marker).unwrap_or("?"),
            rc,
            t.c_lflag
        ));
        1
    }
}