#![no_std]
#![no_main]

use libcluu::*;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // TODO: Start system servers
    // 1. Start procmgr
    // 2. Start vfs
    // 3. Start ramfs
    // 4. Start console
    // 5. Start shell

    // For now, just loop
    loop {
        syscall::sys_yield();
    }
}
