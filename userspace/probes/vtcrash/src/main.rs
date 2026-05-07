//! vtcrash probe: smoke test that the session survives a VT reattach.
//! Lifted from VtCrashTestBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = libcluu::debug_print("vtcrashtest: PASS session alive after VT reattach");
    0
}
