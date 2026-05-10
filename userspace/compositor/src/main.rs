#![no_std]
#![no_main]

extern crate alloc;

mod state;

use libcluu::{debug_print, syscall};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("compositor: stub start");
    loop {
        let _ = syscall::yield_cpu();
    }
}
