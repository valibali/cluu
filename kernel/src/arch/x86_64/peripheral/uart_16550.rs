use core::convert::TryInto;
use syscall::io::{Io, ReadOnly};
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use syscall::pio::Pio;
use bitflags::bitflags;

bitflags! {
    /// Interrupt enable flags
    struct IntEnFlags: u8 {
        const RECEIVED = 1;
        const SENT = 1 << 1;
        const ERRORED = 1 << 2;
        const STATUS_CHANGE = 1 << 3;
        // 4 to 7 are unused
    }
}

bitflags! {
    /// Line status flags
    struct LineStsFlags: u8 {
        const INPUT_FULL = 1;
        // 1 to 4 unknown
        const OUTPUT_EMPTY = 1 << 5;
        // 6 and 7 unknown
    }
}

/// Serial port representation.
pub struct SerialPort<T: Io> {
    data: T,            // Data register, read to receive, write to send
    int_en: T,          // Interrupt enable
    fifo_ctrl: T,       // FIFO control
    line_ctrl: T,       // Line control
    modem_ctrl: T,      // Modem control
    line_sts: ReadOnly<T>,  // Line status
    #[allow(dead_code)]
    modem_sts: ReadOnly<T>, // Modem status, not used right now
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl SerialPort<Pio<u8>> {
    /// Creates a new serial port instance.
    ///
    /// # Arguments
    ///
    /// * `base` - The base port address of the serial port.
    ///
    /// # Returns
    ///
    /// Returns a new `SerialPort` instance.
    pub const fn new(base: u16) -> SerialPort<Pio<u8>> {
        SerialPort {
            data: Pio::new(base),
            int_en: Pio::new(base + 1),
            fifo_ctrl: Pio::new(base + 2),
            line_ctrl: Pio::new(base + 3),
            modem_ctrl: Pio::new(base + 4),
            line_sts: ReadOnly::new(Pio::new(base + 5)),
            modem_sts: ReadOnly::new(Pio::new(base + 6)),
        }
    }
}

impl<T: Io> SerialPort<T>
where
    T::Value: From<u8> + TryInto<u8>,
{
    /// Initializes the serial port.
    pub fn init(&mut self) {
        self.int_en.write(0x00.into());
        self.line_ctrl.write(0x80.into());
        self.data.write(0x01.into());
        self.int_en.write(0x00.into());
        self.line_ctrl.write(0x03.into());
        self.fifo_ctrl.write(0xC7.into());
        self.modem_ctrl.write(0x0B.into());
        self.int_en.write(0x01.into());
    }

    /// Retrieves the line status flags.
    ///
    /// # Returns
    ///
    /// Returns the line status flags indicating the current status of the serial port.
    fn line_sts(&self) -> LineStsFlags {
        LineStsFlags::from_bits_truncate(
            (self.line_sts.read() & 0xFF.into())
                .try_into()
                .unwrap_or(0),
        )
    }

    // pub fn receive(&mut self) -> Option<u8> {
    //     if self.line_sts().contains(LineStsFlags::INPUT_FULL) {
    //         Some(
    //             (self.data.read() & 0xFF.into())
    //                 .try_into()
    //                 .unwrap_or(0),
    //         )
    //     } else {
    //         None
    //     }
    // }

    /// Sends a byte of data through the serial port.
    ///
    /// # Arguments
    ///
    /// * `data` - The data byte to send.
    pub fn send(&mut self, data: u8) {
        while !self.line_sts().contains(LineStsFlags::OUTPUT_EMPTY) {}
        self.data.write(data.into())
    }

    /// Writes a byte of data to the serial port.
    ///
    /// # Arguments
    ///
    /// * `b` - The byte of data to write.
    pub fn write(&mut self, b: u8) {
        match b {
            8 | 0x7F => {
                self.send(8);
                self.send(b' ');
                self.send(8);
            }
            b'\n' => {
                self.send(b'\r');
                self.send(b'\n');
            }
            _ => {
                self.send(b);
            }
        }
    }
}
