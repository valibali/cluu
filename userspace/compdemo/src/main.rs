#![no_std]
#![no_main]

extern crate alloc;

use libcluu::{debug_print, syscall};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("compdemo: stub start");
    loop {
        let _ = syscall::yield_cpu();
    }
}
