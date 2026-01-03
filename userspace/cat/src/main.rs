#![no_std]
#![no_main]

use libcluu::*;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // TODO: Implement cat
    // - Open file via VFS
    // - Read contents
    // - Write to stdout (console)

    // For now, just exit
    0
}
