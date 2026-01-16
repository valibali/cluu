#![no_std]
#![no_main]

// Pull in the shared userspace runtime (panic handler + _start).
#[allow(unused_imports)]
use libcluu::runtime as _;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // TODO: Implement cat
    // - Open file via VFS
    // - Read contents
    // - Write to stdout (console)

    // For now, just exit
    0
}
