//! maperror probe: map_user_page error branch rollback validation.
//! Lifted from MapErrorBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::{syscall, Error};

const TARGET_BASE: usize = 0x6E00_0000;
const MAP_TEST_FAILPOINT: usize = 0x8000_0000;
const MAP_TEST_FAIL_ON_MAP_STAGE: usize = 0x4000_0000;
const MAP_TEST_FAIL_AFTER_SHIFT: usize = 16;
const MAP_TEST_FAIL_AFTER_MASK: usize = 0xFF;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let total_pages = args
        .get(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(3);

    if total_pages < 2 {
        let _ = libcluu::debug_print("maperror: FAIL total_pages must be >= 2");
        return 1;
    }

    let space_token = process_info().tokens[TOKEN_SPACE];
    let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
    let fail_after = 1usize.min(total_pages - 1);
    let fail_bits = (fail_after & MAP_TEST_FAIL_AFTER_MASK) << MAP_TEST_FAIL_AFTER_SHIFT;
    let flags = 0x03 | MAP_TEST_FAILPOINT | MAP_TEST_FAIL_ON_MAP_STAGE | fail_bits;
    let result = syscall::space_map_range(space_token, TARGET_BASE, 0, flags, total_pages, 0);
    match result {
        Err(Error::OutOfMemory) => {}
        Ok(pages) => {
            let line = format!("maperror: FAIL unexpected success pages={}", pages);
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
            return 1;
        }
        Err(err) => {
            let line = format!("maperror: FAIL wrong error {:?}", err);
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
            return 1;
        }
    }

    let verify_result =
        syscall::space_map_range(space_token, TARGET_BASE, 0, 0x03, total_pages, 0);
    match verify_result {
        Ok(mapped) if mapped == total_pages => {}
        Ok(mapped) => {
            let line = format!(
                "maperror: FAIL rollback remap short mapped_pages={}",
                mapped
            );
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
            return 1;
        }
        Err(err) => {
            let line = format!("maperror: FAIL rollback remap error {:?}", err);
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
            return 1;
        }
    }

    let line = format!("maperror: PASS total_pages={}", total_pages);
    let _ = libcluu::debug_print(&line);
    let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
    0
}
