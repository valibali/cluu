/*
 * Port I/O (PIO) Implementation
 *
 * This module provides a safe Rust interface to x86 port I/O operations.
 * Port I/O is the traditional method for communicating with hardware devices
 * on x86 systems, using special CPU instructions (IN/OUT).
 *
 * Why this is important:
 * - Enables communication with legacy hardware devices
 * - Provides type-safe access to I/O ports
 * - Implements the foundation for serial ports, keyboard, etc.
 * - Essential for low-level hardware control
 * - Forms the basis for many device drivers in the kernel
 *
 * The Pio struct provides generic access to I/O ports with compile-time
 * type safety for different data sizes (u8, u16, u32).
 */

use core::{arch::asm, marker::PhantomData};

use super::io::Io;

/// Generic PIO
#[derive(Copy, Clone)]
pub struct Pio<T> {
    port: u16,
    value: PhantomData<T>,
}

impl<T> Pio<T> {
    /// Create a new PIO instance with the specified port.
    ///
    /// # Arguments
    ///
    /// * `port` - The port number.
    ///
    /// # Returns
    ///
    /// A new `Pio` instance.
    pub const fn new(port: u16) -> Self {
        Pio::<T> {
            port,
            value: PhantomData,
        }
    }
}

/// Read/Write for byte PIO
impl Io for Pio<u8> {
    type Value = u8;

    /// Read a byte from the port.
    ///
    /// # Returns
    ///
    /// The read byte value.
    #[inline(always)]
    fn read(&self) -> u8 {
        let value: u8;
        unsafe {
            asm!("in al, dx", in("dx") self.port, out("al") value, options(nostack, nomem, preserves_flags));
        }
        value
    }

    /// Write a byte to the port.
    ///
    /// # Arguments
    ///
    /// * `value` - The byte value to write.
    #[inline(always)]
    fn write(&mut self, value: u8) {
        unsafe {
            asm!("out dx, al", in("dx") self.port, in("al") value, options(nostack, nomem, preserves_flags));
        }
    }
}

/// Read/Write for word PIO
impl Io for Pio<u16> {
    type Value = u16;

    /// Read a word from the port.
    ///
    /// # Returns
    ///
    /// The read word value.
    #[inline(always)]
    fn read(&self) -> u16 {
        let value: u16;
        unsafe {
            asm!("in ax, dx", in("dx") self.port, out("ax") value, options(nostack, nomem, preserves_flags));
        }
        value
    }

    /// Write a word to the port.
    ///
    /// # Arguments
    ///
    /// * `value` - The word value to write.
    #[inline(always)]
    fn write(&mut self, value: u16) {
        unsafe {
            asm!("out dx, ax", in("dx") self.port, in("ax") value, options(nostack, nomem, preserves_flags));
        }
    }
}

/// Read/Write for doubleword PIO
impl Io for Pio<u32> {
    type Value = u32;

    /// Read a doubleword from the port.
    ///
    /// # Returns
    ///
    /// The read doubleword value.
    #[inline(always)]
    fn read(&self) -> u32 {
        let value: u32;
        unsafe {
            asm!("in eax, dx", in("dx") self.port, out("eax") value, options(nostack, nomem, preserves_flags));
        }
        value
    }

    /// Write a doubleword to the port.
    ///
    /// # Arguments
    ///
    /// * `value` - The doubleword value to write.
    #[inline(always)]
    fn write(&mut self, value: u32) {
        unsafe {
            asm!("out dx, eax", in("dx") self.port, in("eax") value, options(nostack, nomem, preserves_flags));
        }
    }
}
