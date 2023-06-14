use core::fmt;
use spin::MutexGuard;
use syscall::pio::Pio;
use arch::x86_64::peripheral::uart_16550::SerialPort;
use arch::x86_64::peripheral::COM2;

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
