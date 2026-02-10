//! PCI (Peripheral Component Interconnect) device access helpers.
//!
//! Provides utilities for PCI configuration space access, device enumeration,
//! and BAR (Base Address Register) management.

use crate::{syscall, Result};

/// PCI configuration space register offsets
pub mod config {
    pub const VENDOR_DEVICE: u8 = 0x00;
    pub const COMMAND_STATUS: u8 = 0x04;
    pub const CLASS_REVISION: u8 = 0x08;
    pub const HEADER_TYPE: u8 = 0x0C;
    pub const BAR0: u8 = 0x10;
    pub const BAR1: u8 = 0x14;
    pub const BAR2: u8 = 0x18;
    pub const BAR3: u8 = 0x1C;
    pub const BAR4: u8 = 0x20;
    pub const BAR5: u8 = 0x24;
    pub const CAP_PTR: u8 = 0x34;
}

/// PCI command register bits
pub mod command {
    pub const IO_SPACE: u16 = 1 << 0;
    pub const MEMORY_SPACE: u16 = 1 << 1;
    pub const BUS_MASTER: u16 = 1 << 2;
}

/// Information about a PCI Base Address Register
#[derive(Debug, Clone, Copy)]
pub struct BarInfo {
    /// Base address (masked, excluding flag bits)
    pub address: u32,
    /// Size of the BAR in bytes
    pub size: u32,
    /// True if this is an I/O port BAR (not MMIO)
    pub is_io: bool,
    /// True if this is a 64-bit MMIO BAR
    pub is_64bit: bool,
    /// True if this is a prefetchable MMIO BAR
    pub is_prefetchable: bool,
}

/// Read from PCI configuration space.
///
/// Requires a token with PCI_ACCESS right.
pub fn config_read_u32(
    pci_token: usize,
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
) -> Result<u32> {
    syscall::pci_config_read(pci_token, bus, device, function, offset)
}

/// Write to PCI configuration space.
///
/// Requires a token with PCI_ACCESS right.
pub fn config_write_u32(
    pci_token: usize,
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
    value: u32,
) -> Result<()> {
    syscall::pci_config_write(pci_token, bus, device, function, offset, value)
}

/// Read vendor and device IDs.
pub fn read_ids(pci_token: usize, bus: u8, device: u8, function: u8) -> Result<(u16, u16)> {
    let vendor_device = config_read_u32(pci_token, bus, device, function, config::VENDOR_DEVICE)?;
    let vendor_id = (vendor_device & 0xFFFF) as u16;
    let device_id = ((vendor_device >> 16) & 0xFFFF) as u16;
    Ok((vendor_id, device_id))
}

/// Parse a BAR (Base Address Register).
///
/// Returns None if the BAR is not implemented (reads as 0).
pub fn parse_bar(bar_value: u32) -> Option<BarInfo> {
    if bar_value == 0 {
        return None;
    }

    let is_io = (bar_value & 1) != 0;

    if is_io {
        // I/O space BAR
        let address = bar_value & 0xFFFF_FFFC;
        Some(BarInfo {
            address,
            size: 0, // Size must be determined separately
            is_io: true,
            is_64bit: false,
            is_prefetchable: false,
        })
    } else {
        // Memory space BAR
        let mem_type = (bar_value >> 1) & 0x3;
        let is_prefetchable = (bar_value & 0x8) != 0;
        let is_64bit = mem_type == 2;
        let address = bar_value & 0xFFFF_FFF0;

        Some(BarInfo {
            address,
            size: 0, // Size must be determined separately
            is_io: false,
            is_64bit,
            is_prefetchable,
        })
    }
}

/// Determine the size of a BAR by writing all 1s and reading back.
///
/// WARNING: This temporarily modifies the BAR and should only be done
/// after ensuring the device is not in use.
pub fn measure_bar_size(
    pci_token: usize,
    bus: u8,
    device: u8,
    function: u8,
    bar_offset: u8,
    original_value: u32,
) -> Result<u32> {
    // Write all 1s to the BAR
    config_write_u32(pci_token, bus, device, function, bar_offset, 0xFFFF_FFFF)?;

    // Read back the value
    let size_mask = config_read_u32(pci_token, bus, device, function, bar_offset)?;

    // Restore original value
    config_write_u32(pci_token, bus, device, function, bar_offset, original_value)?;

    if size_mask == 0 || size_mask == 0xFFFF_FFFF {
        return Ok(0);
    }

    // Calculate size from the mask
    let is_io = (original_value & 1) != 0;
    let mask = if is_io {
        size_mask & 0xFFFF_FFFC
    } else {
        size_mask & 0xFFFF_FFF0
    };

    let size = (!mask).wrapping_add(1);
    Ok(size)
}

/// Enable a PCI device by setting command register bits.
///
/// Typically enables memory space, I/O space, and bus mastering.
pub fn enable_device(
    pci_token: usize,
    bus: u8,
    device: u8,
    function: u8,
    enable_io: bool,
    enable_mem: bool,
    enable_bus_master: bool,
) -> Result<()> {
    let cmd_status = config_read_u32(pci_token, bus, device, function, config::COMMAND_STATUS)?;
    let mut command = (cmd_status & 0xFFFF) as u16;

    if enable_io {
        command |= command::IO_SPACE;
    }
    if enable_mem {
        command |= command::MEMORY_SPACE;
    }
    if enable_bus_master {
        command |= command::BUS_MASTER;
    }

    let new_cmd_status = (command as u32) | (cmd_status & 0xFFFF_0000);
    config_write_u32(
        pci_token,
        bus,
        device,
        function,
        config::COMMAND_STATUS,
        new_cmd_status,
    )?;

    Ok(())
}

/// Check if a PCI function exists by reading the vendor ID.
///
/// Returns false if vendor ID is 0xFFFF (no device present).
pub fn function_exists(pci_token: usize, bus: u8, device: u8, function: u8) -> bool {
    match read_ids(pci_token, bus, device, function) {
        Ok((vendor_id, _)) => vendor_id != 0xFFFF,
        Err(_) => false,
    }
}
