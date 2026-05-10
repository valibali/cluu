#![no_std]
#![no_main]

extern crate alloc;

mod state;
mod shm;

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
    match shm::alloc_frame(8192) {
        Ok((token, bytes)) => {
            let _ = debug_print("compositor: alloc/free smoke start");
            if shm::free_frame(token).is_ok() {
                let _ = debug_print("compositor: alloc/free ok");
            } else {
                let _ = debug_print("compositor: free failed");
            }
            let _ = (bytes,);
        }
        Err(_) => {
            let _ = debug_print("compositor: alloc failed");
        }
    }
    loop {
        let _ = syscall::yield_cpu();
    }
}
