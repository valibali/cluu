use core::fmt;
use spin::MutexGuard;
use syscall::pio::Pio;
use arch::x86_64::peripheral::uart_16550::SerialPort;
use peripherals::COM2;

pub struct Writer<'a> {
    serial: MutexGuard<'a, SerialPort<Pio<u8>>>,
}

impl<'a> Writer<'a> {
    pub fn new() -> Writer<'a> {
        Writer {
            serial: COM2.lock(),
        }
    }

    pub fn write(&mut self, byte: u8) {
        {
            self.serial.write(byte);
        }
    }
}

impl<'a> fmt::Write for Writer<'a> {
    fn write_str(&mut self, s: &str) -> Result<(), fmt::Error> {
        for byte in s.bytes() {
            self.write(byte);
        }
        Ok(())
    }
}
