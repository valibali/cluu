#![no_std]
#![no_main]

extern crate alloc;

mod state;

use libcluu::{debug_print, syscall};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("compositor: init");
    let _comp = match state::Compositor::init() {
        Ok(c) => c,
        Err(_) => {
            let _ = debug_print("compositor: init failed");
            return -1;
        }
    };
    let _ = debug_print("compositor: ready");
    loop {
        let _ = syscall::yield_cpu();
    }
}
