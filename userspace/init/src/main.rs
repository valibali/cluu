#![no_std]
#![no_main]

use libcluu::{boot_info, debug_print, root_token_handle, yield_cpu, Result};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if let Err(_) = run() {
        -1
    } else {
        0
    }
}

fn run() -> Result<()> {
    debug_print("Init: bootinfo ready")?;
    debug_print("Init: root token exposed via boot info page")?;
    debug_print("Init: ready to spawn procmgr via syscalls (TODO)")?;

    let _root_token = root_token_handle();
    let boot = boot_info();
    let _initrd_phys = boot.initrd_phys;
    let _initrd_size = boot.initrd_size;

    loop {
        debug_print("Init: yielding CPU")?;
        yield_cpu()?;
    }
}
