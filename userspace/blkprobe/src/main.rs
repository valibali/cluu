//! End-to-end raw-block smoke test.
//!
//! Subscribes to `blkdev:main`, opens a BLK session, reads sector 0 into a
//! page-aligned scratch buffer, and verifies the result is non-empty. Prints
//! either `blkprobe: ALL OK` (success) or `blkprobe: [FAIL] ...` (failure).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::BlkSessionClient;

const SCRATCH_VA: usize = 0x4400_0000;
const SCRATCH_PAGES: usize = 1; // single 4 KiB page for sector 0 read

fn fail(name: &str) -> i32 {
    let _ = libcluu::debug_print(&format!("blkprobe: [FAIL] {}", name));
    1
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];

    // Page-aligned scratch buffer for the read DMA target.
    if libcluu::syscall::space_map_range(
        space_token,
        SCRATCH_VA,
        0,
        0x03, // R+W
        SCRATCH_PAGES,
        0,
    ).is_err() {
        return fail("space_map_range scratch");
    }

    if libcluu::registry::init("blkprobe").is_err() {
        return fail("registry::init");
    }
    let _ = libcluu::syscall::yield_cpu();

    let blkdev = match libcluu::registry::subscribe_output("blkdev", "main") {
        Ok(ep) => ep,
        Err(_) => return fail("subscribe blkdev:main"),
    };

    let mut client = match BlkSessionClient::open(blkdev) {
        Ok(c) => c,
        Err(_) => return fail("BlkSessionClient::open"),
    };

    let buf = unsafe {
        core::slice::from_raw_parts_mut(SCRATCH_VA as *mut u8, SCRATCH_PAGES * 4096)
    };

    match client.read_blocking(0, buf) {
        // The driver reports `bytes_done` as the device-reported chain
        // total, which includes the 1-byte status descriptor — so a
        // 4096-byte data read returns n=4097 over the wire. Accept any
        // n covering the requested data range.
        Ok(n) if n >= 4096 => {
            if !buf.iter().any(|&b| b != 0) {
                return fail("sector 0 all zeros");
            }
        }
        Ok(n) => return fail(&format!("short read n={}", n)),
        Err(_) => return fail("read_blocking err"),
    }

    let _ = libcluu::debug_print("blkprobe: ALL OK");
    0
}
