/*
 * Display Drivers
 *
 * This module contains drivers for display and graphics hardware,
 * including framebuffer management and graphics operations.
 */

use core::ptr::addr_of_mut;
use spin::Mutex;
use crate::bootboot::{bootboot, fb};

pub mod framebuffer;

pub use framebuffer::FrameBuffer;

/// Mutex-protected static instance of the framebuffer.
pub static FB: Mutex<Option<FrameBuffer>> = Mutex::new(None);

/// Initialize the framebuffer driver
pub fn init() {
    match FrameBuffer::new(
        { addr_of_mut!(fb) } as *mut u32,
        unsafe { bootboot.fb_scanline },
        unsafe { bootboot.fb_width },
        unsafe { bootboot.fb_height },
    ) {
        Ok(instance) => {
            log::info!("Framebuffer mapped.");
            *FB.lock() = Some(instance)
        }
        Err(err) => panic!("{}", err),
    }
}
