#![allow(unused)]
//! PCI device discovery for virtio-blk.

extern crate alloc;

use libcluu::pci;
use libcluu::syscall::{pci_config_read, pci_config_write};
use libcluu::{Error, Result};

/// PCI vendor ID for virtio devices (Red Hat)
const VIRTIO_VENDOR_ID: u16 = 0x1AF4;

/// PCI device IDs for virtio-blk
const VIRTIO_BLK_DEVICE_ID_LEGACY: u16 = 0x1001; // Transitional device
const VIRTIO_BLK_DEVICE_ID_MODERN: u16 = 0x1042; // Non-transitional (1.0+)

/// PCI configuration space offsets (byte offsets; reads are 32-bit aligned in pci_config_read)
const PCI_BAR0: u8 = 0x10;
const PCI_CAP_PTR: u8 = 0x34;
const PCI_COMMAND_STATUS: u8 = 0x04;

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

    /// BAR0 base (masked address)
    pub bar0: u32,
    pub bar0_size: u32,
    /// True if BAR0 is I/O port (not MMIO)
    pub is_io_bar: bool,

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

/// Find a virtio-blk PCI device by scanning buses/devices/functions.
///
/// Enumeration is READ-ONLY: we only read vendor/device IDs during scanning.
/// Once a candidate virtio-blk function is found, we then read BARs and parse capabilities.
pub fn find_virtio_blk(pci_token: usize) -> Result<PciDevice> {
    let _ = libcluu::debug_print("pci: scanning for virtio-blk...");

    // Scan PCI buses 0-7 (OVMF may place devices on higher buses in some topologies)
    for bus in 0..8u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let ids = pci::read_ids(pci_token, bus, device, function);
                if let Ok((vendor_id, device_id)) = ids {
                    let _ = libcluu::debug_print(&alloc::format!(
                        "pci: {:02x}:{:02x}.{} vendor={:04x} device={:04x}",
                        bus,
                        device,
                        function,
                        vendor_id,
                        device_id
                    ));

                    if vendor_id == VIRTIO_VENDOR_ID
                        && (device_id == VIRTIO_BLK_DEVICE_ID_LEGACY
                            || device_id == VIRTIO_BLK_DEVICE_ID_MODERN)
                    {
                        let _ = libcluu::debug_print("pci: found virtio-blk!");
                        return init_device(pci_token, bus, device, function, vendor_id, device_id);
                    }
                }
            }
        }
    }

    let _ = libcluu::debug_print("pci: no virtio-blk found");
    Err(Error::NotFound)
}

/// Full initialization once we know the function is the device we want.
/// Safe to read BARs, size them, and parse virtio caps here.
fn init_device(
    pci_token: usize,
    bus: u8,
    device: u8,
    function: u8,
    vendor_id: u16,
    device_id: u16,
) -> Result<PciDevice> {
    // Read BAR0 (raw)
    let bar0_raw = pci::config_read_u32(pci_token, bus, device, function, PCI_BAR0)?;

    // Parse BAR0 to get address and type
    let bar_info = pci::parse_bar(bar0_raw).ok_or(Error::InvalidState)?;
    let bar0_addr = bar_info.address;
    let is_mmio = !bar_info.is_io;

    // Determine BAR0 size by writing all 1s and reading back (only for the matched device)
    let bar0_size = pci::measure_bar_size(pci_token, bus, device, function, PCI_BAR0, bar0_raw)?;

    let is_modern = device_id == VIRTIO_BLK_DEVICE_ID_MODERN;

    let mut dev = PciDevice {
        bus,
        device,
        function,
        vendor_id,
        device_id,
        bar0: bar0_addr,
        bar0_size,
        is_io_bar: !is_mmio,
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

    // Parse modern virtio capabilities if virtio vendor (applies to both modern and transitional)
    if vendor_id == VIRTIO_VENDOR_ID {
        parse_virtio_caps(pci_token, &mut dev)?;
    }

    Ok(dev)
}

/// Parse virtio PCI capability structures to find MMIO region offsets.
fn parse_virtio_caps(pci_token: usize, dev: &mut PciDevice) -> Result<()> {
    // Status bit 4 (Capabilities List) is in the upper 16 bits of the dword at 0x04.
    let cmd_status = pci_config_read(
        pci_token,
        dev.bus,
        dev.device,
        dev.function,
        PCI_COMMAND_STATUS,
    )?;
    let status16 = (cmd_status >> 16) as u16;
    let has_caps = (status16 & 0x0010) != 0;

    if !has_caps {
        return Ok(());
    }

    // Capabilities pointer is at 0x34 (low byte) for conventional PCI config space.
    let cap_ptr_dword = pci_config_read(pci_token, dev.bus, dev.device, dev.function, PCI_CAP_PTR)?;
    let mut offset = (cap_ptr_dword & 0xFF) as u8;

    // Walk the capabilities list.
    // Each capability header:
    //   offset+0: cap_id (8), next_ptr (8), then capability-specific bytes...
    // We read cap_id and next_ptr from the low 16 bits of the dword at `offset`.
    while offset != 0 && offset != 0xFF {
        let cap_header = pci_config_read(pci_token, dev.bus, dev.device, dev.function, offset)?;
        let cap_id = (cap_header & 0xFF) as u8;
        let next_ptr = ((cap_header >> 8) & 0xFF) as u8;

        if cap_id == PCI_CAP_ID_VENDOR {
            // Virtio uses vendor-specific capability (0x09)
            parse_virtio_cap(pci_token, dev, offset)?;
        }

        offset = next_ptr;
    }

    Ok(())
}

/// Parse a single virtio PCI capability structure.
///
/// Layout (virtio 1.0+):
///  0x00: cap_id (8), cap_next (8), cap_len (8), cfg_type (8)
///  0x04: bar (8), padding (24)
///  0x08: offset (32)
///  0x0C: length (32)
///  0x10: (notify) notify_off_multiplier (32) for NOTIFY cap
fn parse_virtio_cap(pci_token: usize, dev: &mut PciDevice, cap_offset: u8) -> Result<()> {
    let header = pci_config_read(pci_token, dev.bus, dev.device, dev.function, cap_offset)?;
    let cfg_type = ((header >> 24) & 0xFF) as u8;

    let bar_word = pci_config_read(pci_token, dev.bus, dev.device, dev.function, cap_offset + 4)?;
    let bar = (bar_word & 0xFF) as u8;

    let region_offset =
        pci_config_read(pci_token, dev.bus, dev.device, dev.function, cap_offset + 8)?;

    match cfg_type {
        VIRTIO_PCI_CAP_COMMON_CFG => {
            dev.common_cfg_bar = bar;
            dev.common_cfg_offset = region_offset;
        }
        VIRTIO_PCI_CAP_NOTIFY_CFG => {
            dev.notify_cfg_bar = bar;
            dev.notify_cfg_offset = region_offset;

            // notify_off_multiplier at cap_offset + 16
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
            dev.isr_cfg_offset = region_offset;
        }
        VIRTIO_PCI_CAP_DEVICE_CFG => {
            dev.device_cfg_bar = bar;
            dev.device_cfg_offset = region_offset;
        }
        _ => {}
    }

    Ok(())
}

/// Enable bus mastering and memory/IO space access for a PCI device.
pub fn enable_device(pci_token: usize, dev: &PciDevice) -> Result<()> {
    // Enable I/O space (for legacy I/O port devices), memory space (for MMIO), and bus mastering (for DMA)
    pci::enable_device(pci_token, dev.bus, dev.device, dev.function, true, true, true)
}
