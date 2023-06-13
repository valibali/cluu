use core::ptr::addr_of_mut;
use syscall::pio::Pio;
use spin::Mutex;

use self::uart_16550::SerialPort;
use self::framebuffer::*;
use crate::bootboot::*;

pub mod uart_16550;
pub mod framebuffer;

/// Mutex-protected static instance of COM2 serial port.
pub static COM2: Mutex<SerialPort<Pio<u8>>> = Mutex::new(SerialPort::<Pio<u8>>::new(0x2F8));

/// Mutex-protected static instance of the framebuffer.
pub static FB: Mutex<Option<FrameBuffer>> = Mutex::new(None);

/// Initializes the peripherals.
///
/// This function initializes the COM2 serial port and the framebuffer.
pub fn init_peripherals() {
    COM2.lock().init();
    *FB.lock() = Some(FrameBuffer::new(
        unsafe { addr_of_mut!(fb) } as *mut u32,
        unsafe { bootboot.fb_scanline },
        unsafe { bootboot.fb_width },
        unsafe { bootboot.fb_height },
    ));
}
