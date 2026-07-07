//! stackgrow probe: M10 stack-growth demand-paging validation.
//!
//! The kernel demand-pages faults in USER_STACK_BOTTOM..USER_STACK_TOP
//! (0x7f00_0000..0x80_00_0000, 16 MB) with read+write+no-exec and warns
//! at 1/4/8 MB growth thresholds. This probe exercises that path by
//! touching pages going down from USER_STACK_TOP, simulating stack growth.
//!
//! Modes (argv[1]):
//!   grow     (default) — touch `depth` pages (default 1000 = ~4 MB), verify
//!   overflow            — touch 5000 pages (~20 MB), expect thread kill
//!                        by the USER_STACK_BOTTOM guard page (no PASS output)

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::debug_print;

const USER_STACK_TOP: usize = 0x8000_0000;
const PAGE_SIZE: usize = 4096;
const SENTINEL: u64 = 0xDEAD_BEEF_CAFE_BABE;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("grow");
    let depth = args
        .get(2)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1000);

    if mode == "overflow" {
        return run_overflow();
    }

    run_grow(depth)
}

fn run_grow(depth: usize) -> i32 {
    if depth == 0 {
        let _ = debug_print("stackgrow: FAIL depth must be > 0");
        return 1;
    }

    let mut touched = 0usize;
    for i in 0..depth {
        let addr = (USER_STACK_TOP - (i + 1) * PAGE_SIZE) as *mut u64;
        unsafe { core::ptr::write_volatile(addr, SENTINEL.wrapping_add(i as u64)) };
        touched += 1;
    }

    // Verify a sampling of written pages survived (demand paging preserved them).
    let samples = [0usize, depth / 2, depth - 1];
    for &i in &samples {
        let addr = (USER_STACK_TOP - (i + 1) * PAGE_SIZE) as *const u64;
        let val = unsafe { core::ptr::read_volatile(addr) };
        if val != SENTINEL.wrapping_add(i as u64) {
            let _ = debug_print("stackgrow: FAIL verify mismatch");
            return 1;
        }
    }

    let msg = format!("stackgrow: PASS touched={} pages ({:#x} bytes)", touched, touched * PAGE_SIZE);
    let _ = debug_print(&msg);
    0
}

fn run_overflow() -> i32 {
    // 5000 pages = ~20 MB > 16 MB USER_STACK_SIZE. The page at USER_STACK_BOTTOM
    // (0x7f00_0000) is the guard page and is NOT demand-paged; touching it kills
    // the thread. If we reach the PASS line, demand paging over-grew the stack.
    let depth = 5000usize;
    for i in 0..depth {
        let addr = (USER_STACK_TOP - (i + 1) * PAGE_SIZE) as *mut u64;
        unsafe { core::ptr::write_volatile(addr, SENTINEL) };
    }
    let _ = debug_print("stackgrow: FAIL overflow not caught");
    1
}
