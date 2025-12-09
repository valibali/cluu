/*
 * Peripheral Device Management
 *
 * This module manages all peripheral devices and hardware interfaces
 * available to the kernel. It provides initialization and access to
 * various hardware components like serial ports and framebuffers.
 *
 * Why this is important:
 * - Centralizes all hardware device management
 * - Provides safe, synchronized access to hardware resources
 * - Implements proper initialization sequences for devices
 * - Enables early debug output and graphics capabilities
 * - Forms the foundation for all hardware interaction in the kernel
 *
 * Key peripherals managed:
 * - COM2 serial port for debug logging
 * - Framebuffer for graphics output
 * - Future expansion for other hardware devices
 */

use core::ptr::addr_of_mut;
use spin::Mutex;

use crate::bootboot::{bootboot, fb};
use crate::syscall::pio::Pio;

use self::framebuffer::FrameBuffer;
use self::uart_16550::SerialPort;

pub mod framebuffer;
pub mod pic;
pub mod uart_16550;

/// Mutex-protected static instance of COM2 serial port.
pub static COM2: Mutex<SerialPort<Pio<u8>>> = Mutex::new(SerialPort::<Pio<u8>>::new(0x2F8));

/// Mutex-protected static instance of the framebuffer.
pub static FB: Mutex<Option<FrameBuffer>> = Mutex::new(None);

/// Initializes the debug port (COM2) for logging.
///
/// This function should be called early in the boot process before any logging occurs.
pub fn init_debug_port() {
    COM2.lock().init();
}

/// Initializes the peripherals (excluding debug ports).
///
/// This function initializes the framebuffer and other peripherals,
/// but excludes debug infrastructure which should be initialized earlier.
pub fn init_peripherals() {
    // Initialize framebuffer
    init_framebuffer();
    log::info!("Framebuffer initialization complete");
    pic::init_pic();
    log::info!("PIC initialization complete");
}

fn init_framebuffer() {
    match FrameBuffer::new(
        { addr_of_mut!(fb) } as *mut u32,
        unsafe { bootboot.fb_scanline },
        unsafe { bootboot.fb_width },
        unsafe { bootboot.fb_height },
    ) {
        Ok(instace) => {
            log::info!("Framebuffer mapped.");
            *FB.lock() = Some(instace)
        }
        Err(err) => panic!("{}", err),
    }
}
