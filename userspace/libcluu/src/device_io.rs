//! Device I/O abstraction for MMIO and I/O port access.
//!
//! Provides a unified interface for device register access that works with both
//! MMIO (Memory-Mapped I/O) and I/O ports, following SOLID principles.

use crate::syscall;
use core::sync::atomic::{fence, Ordering};

/// Trait for device register access.
///
/// Implementations provide either MMIO or I/O port access with the same interface,
/// allowing drivers to work with both access methods transparently.
pub trait DeviceIo: Send + Sync {
    /// Read 8-bit value from device register
    fn read_u8(&self, offset: u16) -> u8;

    /// Write 8-bit value to device register
    fn write_u8(&self, offset: u16, value: u8);

    /// Read 16-bit value from device register
    fn read_u16(&self, offset: u16) -> u16;

    /// Write 16-bit value to device register
    fn write_u16(&self, offset: u16, value: u16);

    /// Read 32-bit value from device register
    fn read_u32(&self, offset: u16) -> u32;

    /// Write 32-bit value to device register
    fn write_u32(&self, offset: u16, value: u32);
}

/// MMIO (Memory-Mapped I/O) device access.
///
/// Accesses device registers through memory-mapped addresses.
pub struct MmioRegion {
    base: usize,
}

impl MmioRegion {
    /// Create a new MMIO region at the given virtual address.
    ///
    /// # Safety
    /// The caller must ensure that `base` points to valid mapped device memory.
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    /// Get the base address of this MMIO region.
    pub fn base(&self) -> usize {
        self.base
    }
}

impl DeviceIo for MmioRegion {
    fn read_u8(&self, offset: u16) -> u8 {
        unsafe {
            let ptr = (self.base + offset as usize) as *const u8;
            ptr.read_volatile()
        }
    }

    fn write_u8(&self, offset: u16, value: u8) {
        unsafe {
            let ptr = (self.base + offset as usize) as *mut u8;
            ptr.write_volatile(value);
        }
        fence(Ordering::SeqCst);
    }

    fn read_u16(&self, offset: u16) -> u16 {
        unsafe {
            let ptr = (self.base + offset as usize) as *const u16;
            ptr.read_volatile()
        }
    }

    fn write_u16(&self, offset: u16, value: u16) {
        unsafe {
            let ptr = (self.base + offset as usize) as *mut u16;
            ptr.write_volatile(value);
        }
        fence(Ordering::SeqCst);
    }

    fn read_u32(&self, offset: u16) -> u32 {
        unsafe {
            let ptr = (self.base + offset as usize) as *const u32;
            ptr.read_volatile()
        }
    }

    fn write_u32(&self, offset: u16, value: u32) {
        unsafe {
            let ptr = (self.base + offset as usize) as *mut u32;
            ptr.write_volatile(value);
        }
        fence(Ordering::SeqCst);
    }
}

/// I/O port device access.
///
/// Accesses device registers through x86 I/O ports using IN/OUT instructions.
pub struct PortIoRegion {
    base: u16,
    pci_token: usize,
}

impl PortIoRegion {
    /// Create a new I/O port region at the given port base address.
    ///
    /// Requires a token with PCI_ACCESS right for I/O port operations.
    pub const fn new(base: u16, pci_token: usize) -> Self {
        Self { base, pci_token }
    }

    /// Get the base port address of this I/O region.
    pub fn base(&self) -> u16 {
        self.base
    }
}

impl DeviceIo for PortIoRegion {
    fn read_u8(&self, offset: u16) -> u8 {
        let port = self.base.wrapping_add(offset);
        syscall::port_in8(self.pci_token, port).unwrap_or(0)
    }

    fn write_u8(&self, offset: u16, value: u8) {
        let port = self.base.wrapping_add(offset);
        let _ = syscall::port_out8(self.pci_token, port, value);
        fence(Ordering::SeqCst);
    }

    fn read_u16(&self, offset: u16) -> u16 {
        let port = self.base.wrapping_add(offset);
        syscall::port_in16(self.pci_token, port).unwrap_or(0)
    }

    fn write_u16(&self, offset: u16, value: u16) {
        let port = self.base.wrapping_add(offset);
        let _ = syscall::port_out16(self.pci_token, port, value);
        fence(Ordering::SeqCst);
    }

    fn read_u32(&self, offset: u16) -> u32 {
        let port = self.base.wrapping_add(offset);
        syscall::port_in32(self.pci_token, port).unwrap_or(0)
    }

    fn write_u32(&self, offset: u16, value: u32) {
        let port = self.base.wrapping_add(offset);
        let _ = syscall::port_out32(self.pci_token, port, value);
        fence(Ordering::SeqCst);
    }
}
