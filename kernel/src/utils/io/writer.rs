/*
 * Serial Console Writer
 *
 * This module provides a writer interface for outputting text to the
 * serial console. It implements the core::fmt::Write trait to enable
 * formatted output through the serial port.
 *
 * Why this is important:
 * - Provides the foundation for all kernel text output
 * - Enables early debugging before graphics are available
 * - Implements thread-safe access to the serial port
 * - Forms the basis for the logging and print macro systems
 * - Essential for kernel development and debugging
 *
 * The writer uses the COM2 serial port and provides formatted output
 * capabilities compatible with Rust's formatting system.
 */

use core::fmt;

use spin::MutexGuard;

use crate::drivers::serial::{SerialPort, COM2};
use crate::io::Pio;

/// A simple writer that writes to the serial port.
pub struct Writer<'a> {
    serial: MutexGuard<'a, SerialPort<Pio<u8>>>,
}

impl<'a> Writer<'a> {
    /// Creates a new instance of the writer.
    ///
    /// # Example
    ///
    /// ```rust
    /// let writer = Writer::new();
    /// ```
    pub fn new() -> Writer<'a> {
        Writer {
            serial: COM2.lock(),
        }
    }

    /// Writes a byte to the serial port.
    ///
    /// # Arguments
    ///
    /// * `byte` - The byte to write.
    ///
    /// # Example
    ///
    /// ```rust
    /// let mut writer = Writer::new();
    /// writer.write(b'A');
    /// ```
    pub fn write(&mut self, byte: u8) {
        {
            self.serial.write(byte);
        }
    }
}

impl<'a> fmt::Write for Writer<'a> {
    /// Writes a string to the serial port.
    ///
    /// # Arguments
    ///
    /// * `s` - The string to write.
    ///
    /// # Example
    ///
    /// ```rust
    /// let mut writer = Writer::new();
    /// writer.write_str("Hello, World!");
    /// ```
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write(byte);
        }
        Ok(())
    }
}
