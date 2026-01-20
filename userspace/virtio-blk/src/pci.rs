#![allow(unused)]
//! PCI device discovery for virtio-blk.

use libcluu::syscall::pci_config_read;
use libcluu::{Error, Result};

/// PCI vendor ID for virtio devices (Red Hat)
const VIRTIO_VENDOR_ID: u16 = 0x1AF4;

/// PCI device IDs for virtio-blk
const VIRTIO_BLK_DEVICE_ID_LEGACY: u16 = 0x1001; // Transitional device
const VIRTIO_BLK_DEVICE_ID_MODERN: u16 = 0x1042; // Non-transitional (1.0+)

/// PCI configuration space offsets
const PCI_VENDOR_ID: u8 = 0x00;
const PCI_DEVICE_ID: u8 = 0x02;
const PCI_COMMAND: u8 = 0x04;
const PCI_STATUS: u8 = 0x06;
const PCI_CLASS_REVISION: u8 = 0x08;
const PCI_BAR0: u8 = 0x10;
const PCI_CAP_PTR: u8 = 0x34;

/// PCI capability IDs
const PCI_CAP_ID_VENDOR: u8 = 0x09;

/// Virtio PCI capability types
pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

/// Discovered PCI device information
#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub bar0: u32,
    pub bar0_size: u32,
    /// Offset of common config capability in BAR
    pub common_cfg_offset: u32,
    pub common_cfg_bar: u8,
    /// Offset of notify capability in BAR
    pub notify_cfg_offset: u32,
    pub notify_cfg_bar: u8,
    pub notify_off_multiplier: u32,
    /// Offset of device-specific config in BAR
    pub device_cfg_offset: u32,
    pub device_cfg_bar: u8,
    /// Offset of ISR config in BAR
    pub isr_cfg_offset: u32,
    pub isr_cfg_bar: u8,
    /// True if device supports modern virtio (1.0+)
    pub is_modern: bool,
}

/// Find a virtio-blk PCI device by scanning the bus.
pub fn find_virtio_blk(pci_token: usize) -> Result<PciDevice> {
    let _ = libcluu::debug_print("pci: scanning for virtio-blk...");

    // Scan PCI buses 0-7 (OVMF may place virtio on higher buses)
    for bus in 0..8u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                if let Ok(dev) = probe_device(pci_token, bus, device, function) {
                    let _ = libcluu::debug_print(&alloc::format!(
                        "pci: {:02x}:{:02x}.{} vendor={:04x} device={:04x}",
                        bus,
                        device,
                        function,
                        dev.vendor_id,
                        dev.device_id
                    ));
                    // Check if it's a virtio-blk device
                    if dev.vendor_id == VIRTIO_VENDOR_ID
                        && (dev.device_id == VIRTIO_BLK_DEVICE_ID_LEGACY
                            || dev.device_id == VIRTIO_BLK_DEVICE_ID_MODERN)
                    {
                        let _ = libcluu::debug_print("pci: found virtio-blk!");
                        return Ok(dev);
                    }
                }
            }
        }
    }

    let _ = libcluu::debug_print("pci: no virtio-blk found");
    Err(Error::NotFound)
}

/// Probe a specific PCI device location.
fn probe_device(pci_token: usize, bus: u8, device: u8, function: u8) -> Result<PciDevice> {
    // Read vendor/device ID
    let vendor_device = pci_config_read(pci_token, bus, device, function, PCI_VENDOR_ID)?;
    let vendor_id = (vendor_device & 0xFFFF) as u16;
    let device_id = ((vendor_device >> 16) & 0xFFFF) as u16;

    // 0xFFFF means no device present
    if vendor_id == 0xFFFF {
        return Err(Error::NotFound);
    }

    // Read BAR0
    let bar0 = pci_config_read(pci_token, bus, device, function, PCI_BAR0)?;

    // Determine if BAR0 is memory-mapped (bit 0 = 0) or I/O (bit 0 = 1)
    let is_mmio = (bar0 & 1) == 0;
    let bar0_addr = if is_mmio {
        bar0 & 0xFFFFFFF0
    } else {
        bar0 & 0xFFFFFFFC
    };

    // Determine BAR0 size by writing all 1s and reading back
    let bar0_size = determine_bar_size(pci_token, bus, device, function, PCI_BAR0, bar0)?;

    let is_modern = device_id == VIRTIO_BLK_DEVICE_ID_MODERN;

    let mut dev = PciDevice {
        bus,
        device,
        function,
        vendor_id,
        device_id,
        bar0: bar0_addr,
        bar0_size,
        common_cfg_offset: 0,
        common_cfg_bar: 0,
        notify_cfg_offset: 0,
        notify_cfg_bar: 0,
        notify_off_multiplier: 0,
        device_cfg_offset: 0,
        device_cfg_bar: 0,
        isr_cfg_offset: 0,
        isr_cfg_bar: 0,
        is_modern,
    };

    // For modern virtio devices, parse capability structures
    if vendor_id == VIRTIO_VENDOR_ID {
        parse_virtio_caps(pci_token, &mut dev)?;
    }

    Ok(dev)
}

/// Determine the size of a BAR by writing all 1s and reading back.
fn determine_bar_size(
    pci_token: usize,
    bus: u8,
    device: u8,
    function: u8,
    bar_offset: u8,
    original: u32,
) -> Result<u32> {
    // Write all 1s
    libcluu::syscall::pci_config_write(pci_token, bus, device, function, bar_offset, 0xFFFFFFFF)?;

    // Read back
    let size_mask = pci_config_read(pci_token, bus, device, function, bar_offset)?;

    // Restore original value
    libcluu::syscall::pci_config_write(pci_token, bus, device, function, bar_offset, original)?;

    // Calculate size from mask
    let is_mmio = (original & 1) == 0;
    let mask = if is_mmio {
        size_mask & 0xFFFFFFF0
    } else {
        size_mask & 0xFFFFFFFC
    };

    if mask == 0 {
        return Ok(0);
    }

    // Size is (~mask) + 1
    let size = (!mask).wrapping_add(1);
    Ok(size)
}

/// Parse virtio PCI capability structures to find MMIO region offsets.
fn parse_virtio_caps(pci_token: usize, dev: &mut PciDevice) -> Result<()> {
    // Read status register to check if capabilities list is valid
    let status = pci_config_read(pci_token, dev.bus, dev.device, dev.function, PCI_STATUS)?;
    let has_caps = ((status >> 16) & 0x10) != 0;

    if !has_caps {
        return Ok(());
    }

    // Read capabilities pointer
    let cap_ptr = pci_config_read(pci_token, dev.bus, dev.device, dev.function, PCI_CAP_PTR)?;
    let mut offset = (cap_ptr & 0xFF) as u8;

    // Walk the capabilities list
    while offset != 0 && offset != 0xFF {
        let cap_header = pci_config_read(pci_token, dev.bus, dev.device, dev.function, offset)?;
        let cap_id = (cap_header & 0xFF) as u8;
        let next_ptr = ((cap_header >> 8) & 0xFF) as u8;

        if cap_id == PCI_CAP_ID_VENDOR {
            // This is a virtio capability
            parse_virtio_cap(pci_token, dev, offset)?;
        }

        offset = next_ptr;
    }

    Ok(())
}

/// Parse a single virtio PCI capability structure.
fn parse_virtio_cap(pci_token: usize, dev: &mut PciDevice, cap_offset: u8) -> Result<()> {
    // Virtio capability structure:
    // Offset 0: cap_id (8), cap_next (8), cap_len (8), cfg_type (8)
    // Offset 4: bar (8), padding (24)
    // Offset 8: offset (32)
    // Offset 12: length (32)

    let header = pci_config_read(pci_token, dev.bus, dev.device, dev.function, cap_offset)?;
    let cfg_type = ((header >> 24) & 0xFF) as u8;

    let bar_word = pci_config_read(pci_token, dev.bus, dev.device, dev.function, cap_offset + 4)?;
    let bar = (bar_word & 0xFF) as u8;

    let offset = pci_config_read(pci_token, dev.bus, dev.device, dev.function, cap_offset + 8)?;

    match cfg_type {
        VIRTIO_PCI_CAP_COMMON_CFG => {
            dev.common_cfg_bar = bar;
            dev.common_cfg_offset = offset;
        }
        VIRTIO_PCI_CAP_NOTIFY_CFG => {
            dev.notify_cfg_bar = bar;
            dev.notify_cfg_offset = offset;
            // Read notify_off_multiplier at offset 16
            dev.notify_off_multiplier = pci_config_read(
                pci_token,
                dev.bus,
                dev.device,
                dev.function,
                cap_offset + 16,
            )?;
        }
        VIRTIO_PCI_CAP_ISR_CFG => {
            dev.isr_cfg_bar = bar;
            dev.isr_cfg_offset = offset;
        }
        VIRTIO_PCI_CAP_DEVICE_CFG => {
            dev.device_cfg_bar = bar;
            dev.device_cfg_offset = offset;
        }
        _ => {}
    }

    Ok(())
}

/// Enable bus mastering and memory space access for a PCI device.
pub fn enable_device(pci_token: usize, dev: &PciDevice) -> Result<()> {
    let cmd = pci_config_read(pci_token, dev.bus, dev.device, dev.function, PCI_COMMAND)?;
    // Set bits: Memory Space Enable (1), Bus Master Enable (2)
    let new_cmd = cmd | 0x06;
    libcluu::syscall::pci_config_write(
        pci_token,
        dev.bus,
        dev.device,
        dev.function,
        PCI_COMMAND,
        new_cmd,
    )
}
