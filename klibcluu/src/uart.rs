//! UART 16550 Serial Port Driver
//!
//! Provides IRQ-safe serial port output for kernel logging.
//! Uses trait-based abstraction for portability across architectures.
//!
//! # Architecture
//!
//! - `PortIo` trait: Abstraction for port I/O operations
//! - `X86PortIo`: x86_64 implementation using x86_64 crate
//! - `Uart<P>`: Generic UART driver over any PortIo implementation
//!
//! # SOLID Principles
//!
//! - **Single Responsibility**: PortIo handles I/O, Uart handles protocol
//! - **Open/Closed**: New architectures can be added without modifying Uart
//! - **Liskov Substitution**: Any PortIo implementation works with Uart
//! - **Interface Segregation**: Minimal, focused trait interface
//! - **Dependency Inversion**: Uart depends on abstraction, not concrete type

use core::marker::PhantomData;

/// Port I/O abstraction trait
///
/// Allows UART driver to work across different architectures.
/// Implementations must be IRQ-safe (no locks, no allocation).
pub trait PortIo {
    /// Read a byte from the specified port
    ///
    /// # Safety
    ///
    /// Caller must ensure the port is valid and accessible.
    unsafe fn read_u8(port: u16) -> u8;

    /// Write a byte to the specified port
    ///
    /// # Safety
    ///
    /// Caller must ensure the port is valid and accessible.
    unsafe fn write_u8(port: u16, value: u8);
}

/// x86_64 port I/O implementation using x86_64 crate
#[cfg(target_arch = "x86_64")]
pub struct X86PortIo;

#[cfg(target_arch = "x86_64")]
impl PortIo for X86PortIo {
    #[inline]
    unsafe fn read_u8(port: u16) -> u8 {
        use x86_64::instructions::port::Port;
        let mut port = Port::new(port);
        port.read()
    }

    #[inline]
    unsafe fn write_u8(port: u16, value: u8) {
        use x86_64::instructions::port::Port;
        let mut port = Port::new(port);
        port.write(value);
    }
}

/// UART 16550 Serial Port
///
/// Generic over port I/O implementation for portability.
pub struct Uart<P: PortIo> {
    base: u16,
    _phantom: PhantomData<P>,
}

impl<P: PortIo> Uart<P> {
    /// COM1 base port (0x3F8)
    pub const COM1_BASE: u16 = 0x3F8;

    /// COM2 base port (0x2F8)
    pub const COM2_BASE: u16 = 0x2F8;

    /// Create a new UART instance
    ///
    /// # Safety
    ///
    /// Must only be called once for each port, and the port must exist.
    pub const fn new(base: u16) -> Self {
        Self {
            base,
            _phantom: PhantomData,
        }
    }

    /// Initialize the UART port
    ///
    /// Sets up 8N1 (8 data bits, no parity, 1 stop bit) at 115200 baud.
    ///
    /// # Safety
    ///
    /// Must be called before using the UART.
    pub unsafe fn init(&self) {
        // Disable interrupts
        self.write_port(1, 0x00);

        // Enable DLAB (set baud rate divisor)
        self.write_port(3, 0x80);

        // Set divisor to 1 (115200 baud)
        self.write_port(0, 0x01); // Divisor low byte
        self.write_port(1, 0x00); // Divisor high byte

        // 8 bits, no parity, one stop bit (8N1)
        self.write_port(3, 0x03);

        // Enable FIFO, clear them, with 14-byte threshold
        self.write_port(2, 0xC7);

        // IRQs enabled, RTS/DSR set
        self.write_port(4, 0x0B);

        // Set in loopback mode, test the serial chip
        self.write_port(4, 0x1E);

        // Test serial chip (send byte 0xAE and check if we get it back)
        self.write_port(0, 0xAE);

        // Check if serial is faulty (i.e., not same byte as sent)
        if self.read_port(0) != 0xAE {
            // Serial port is faulty, but continue anyway
            // (better to have broken serial than no logging at all)
        }

        // Set in normal operation mode (IRQs enabled, RTS/DSR set)
        self.write_port(4, 0x0F);
    }

    /// Write a byte to the UART
    ///
    /// This is IRQ-safe and does not use locks.
    #[inline]
    pub fn write_byte(&self, byte: u8) {
        unsafe {
            // Wait for transmit buffer to be empty
            while (self.read_port(5) & 0x20) == 0 {}

            // Write the byte
            self.write_port(0, byte);
        }
    }

    /// Write a string to the UART
    ///
    /// This is IRQ-safe and does not use locks.
    /// Converts '\n' to '\r\n' for proper terminal display.
    pub fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            // Convert \n to \r\n for proper terminal display
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
    }

    /// Read from port (offset from base)
    #[inline]
    unsafe fn read_port(&self, offset: u16) -> u8 {
        P::read_u8(self.base + offset)
    }

    /// Write to port (offset from base)
    #[inline]
    unsafe fn write_port(&self, offset: u16, value: u8) {
        P::write_u8(self.base + offset, value);
    }
}

/// Global COM2 UART instance for x86_64
///
/// This is safe to access from multiple contexts because:
/// 1. write_byte/write_str don't use any shared state
/// 2. UART hardware handles concurrent writes safely (at worst, output may interleave)
/// 3. No Mutex or allocation required
///
/// Uses x86_64 port I/O implementation.
#[cfg(target_arch = "x86_64")]
pub static COM2: Uart<X86PortIo> = Uart::new(Uart::<X86PortIo>::COM2_BASE);

/// Initialize UART (COM2)
///
/// Must be called early in kernel boot, before logging.
///
/// # Safety
///
/// Must only be called once during kernel initialization.
#[cfg(target_arch = "x86_64")]
pub unsafe fn init() {
    COM2.init();
}

// For non-x86_64 architectures, provide stub implementations
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn init() {
    // TODO: Implement for other architectures
}

#[cfg(not(target_arch = "x86_64"))]
pub struct DummyUart;

#[cfg(not(target_arch = "x86_64"))]
impl DummyUart {
    pub fn write_byte(&self, _byte: u8) {}
    pub fn write_str(&self, _s: &str) {}
}

#[cfg(not(target_arch = "x86_64"))]
pub static COM2: DummyUart = DummyUart;
