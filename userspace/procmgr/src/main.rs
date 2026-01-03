#![no_std]
#![no_main]

use libcluu::*;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // TODO: Implement process manager
    // - Handle process creation requests
    // - Load ELF files
    // - Manage address spaces

    loop {
        let mut msg = Message::new(0, [0; 6], 0);
        let _ = recv(0, &mut msg, IpcFlags::empty());

        // Handle message
    }
}
