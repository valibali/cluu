#![no_std]
#![no_main]

//! Virtio-gpu PCI 2D driver for CLUU.
//!
//! Classic 2D only — no virgl, blobs, or cursor commands.
//! Implements: GET_DISPLAY_INFO, CREATE_2D, ATTACH/DETACH_BACKING (SG),
//! SET_SCANOUT, TRANSFER_TO_HOST_2D, RESOURCE_FLUSH, UNREF_RESOURCE.
//! Fences on TRANSFER and FLUSH; event processing for display changes.

extern crate alloc;

pub mod driver;
pub mod protocol;

pub use driver::{DisplayMode, GpuDriver};

use libcluu::{debug_print, Result};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&alloc::format!("virtio-gpu: error {:?}", e));
            -1
        }
    }
}

fn run() -> Result<()> {
    let mut driver = GpuDriver::init()?;

    driver.self_test()?;

    driver.publish()?;

    driver.run_loop()
}
