//! Smoke test: echoes argv via `debug_print` so the `l2_argv` harness can
//! verify the argv wire format end-to-end (shell → procmgr CONTAINER_RUN
//! ARGV trailer → ProcessInfo params → libcluu::args decode).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let _ = libcluu::debug_print(&format!("argvprobe: argc={}", args.len()));
    for (i, a) in args.iter().enumerate() {
        let _ = libcluu::debug_print(&format!("argvprobe: arg{}={}", i, a));
    }
    0
}
