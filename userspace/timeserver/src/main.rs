#![no_std]
#![no_main]

use libcluu::{debug_print, yield_cpu, Result};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    debug_print("timeserver: ready")?;

    loop {
        // Placeholder service loop; will handle time requests once wired.
        yield_cpu()?;
    }
}
