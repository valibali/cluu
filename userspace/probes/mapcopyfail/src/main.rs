//! mapcopyfail probe: copy_from_user failure branch rollback validation.
//! Lifted from MapCopyFailBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::{syscall, Error};

const SOURCE_BASE: usize = 0x7100_0000;
const TARGET_BASE: usize = 0x7110_0000;
const SOURCE_PAGES: usize = 2;
const PAGE_SIZE: usize = 4096;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let total_pages = args
        .get(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4);

    if total_pages < 2 {
        let _ = libcluu::debug_print("mapcpfail: FAIL total_pages must be >= 2");
        return 1;
    }

    let space_token = process_info().tokens[TOKEN_SPACE];
    let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
    let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);

    if let Err(err) = syscall::space_map_range(space_token, SOURCE_BASE, 0, 0x03, 1, 0) {
        let line = format!("mapcpfail: FAIL source map error {:?}", err);
        let _ = libcluu::debug_print(&line);
        let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
        return 1;
    }

    let result = syscall::space_map_range(
        space_token,
        TARGET_BASE,
        SOURCE_BASE,
        0x03,
        total_pages,
        PAGE_SIZE * 2,
    );
    match result {
        Err(Error::InvalidAddress) => {}
        Ok(pages) => {
            let line = format!("mapcpfail: FAIL unexpected success pages={}", pages);
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
            let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
            return 1;
        }
        Err(err) => {
            let line = format!("mapcpfail: FAIL wrong error {:?}", err);
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
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
                "mapcpfail: FAIL rollback remap short mapped_pages={}",
                mapped
            );
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
            let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
            return 1;
        }
        Err(err) => {
            let line = format!("mapcpfail: FAIL rollback remap error {:?}", err);
            let _ = libcluu::debug_print(&line);
            let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
            let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
            return 1;
        }
    }

    let line = format!("mapcpfail: PASS total_pages={}", total_pages);
    let _ = libcluu::debug_print(&line);
    let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
    let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
    0
}
