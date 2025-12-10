/*
 * Serial Communication Drivers
 *
 * This module contains drivers for serial communication interfaces,
 * primarily UART-based devices used for debugging and communication.
 */

use spin::Mutex;
use crate::io::Pio;

pub mod uart_16550;

pub use uart_16550::SerialPort;

/// Mutex-protected static instance of COM2 serial port.
pub static COM2: Mutex<SerialPort<Pio<u8>>> = Mutex::new(SerialPort::<Pio<u8>>::new(0x2F8));

/// Initializes the debug port (COM2) for logging.
///
/// This function should be called early in the boot process before any logging occurs.
pub fn init_debug_port() {
    COM2.lock().init();
}
