//! mapfail probe: kernel map-range failpoint rollback validation.
//! Lifted from MapFailBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::{syscall, Error};

const TEST_BASE: usize = 0x6C00_0000;
const MAP_TEST_FAILPOINT: usize = 0x8000_0000;
const MAP_TEST_FAIL_AFTER_SHIFT: usize = 16;
const MAP_TEST_FAIL_AFTER_MASK: usize = 0xFF;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let total_pages = args
        .get(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(12);
    let fail_after_raw = args
        .get(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4);

    if total_pages < 2 {
        let _ = libcluu::debug_print("mapfail: FAIL total_pages must be >= 2");
        return 1;
    }
    let fail_after = fail_after_raw.clamp(1, total_pages - 1);
    let fail_bits = (fail_after & MAP_TEST_FAIL_AFTER_MASK) << MAP_TEST_FAIL_AFTER_SHIFT;
    let flags = 0x03 | MAP_TEST_FAILPOINT | fail_bits;
    let space_token = process_info().tokens[TOKEN_SPACE];

    let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);

    let result = syscall::space_map_range(space_token, TEST_BASE, 0, flags, total_pages, 0);
    match result {
        Err(Error::OutOfMemory) => {}
        Ok(pages) => {
            let line = format!("mapfail: FAIL unexpected success pages={}", pages);
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);
            return 1;
        }
        Err(err) => {
            let line = format!("mapfail: FAIL wrong error {:?}", err);
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);
            return 1;
        }
    }

    let verify_result =
        syscall::space_map_range(space_token, TEST_BASE, 0, 0x03, total_pages, 0);
    match verify_result {
        Ok(mapped) if mapped == total_pages => {}
        Ok(mapped) => {
            let line = format!(
                "mapfail: FAIL rollback remap short mapped_pages={}",
                mapped
            );
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);
            return 1;
        }
        Err(err) => {
            let line = format!("mapfail: FAIL rollback remap error {:?}", err);
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);
            return 1;
        }
    }

    let line = format!(
        "mapfail: PASS total_pages={} fail_after={}",
        total_pages, fail_after
    );
    let _ = libcluu::debug_print(&line);
    let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);
    0
}
