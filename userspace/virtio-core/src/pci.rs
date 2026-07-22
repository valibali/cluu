#![allow(unused)]
//! PCI device discovery for virtio (modern + transitional).
//!
//! Generalized from the original virtio-blk/src/pci.rs so any virtio class
//! can reuse it. Callers pass the device IDs they accept and which of those
//! correspond to the modern (1.0+) interface.

extern crate alloc;

use libcluu::ipc::{PARAM_DEVICE_PATH, PARAM_IRQ_LINE};
use libcluu::pci;
use libcluu::syscall::{pci_config_read, pci_config_write};
use libcluu::{Error, Result};

/// PCI vendor ID for virtio devices (Red Hat).
pub const VIRTIO_VENDOR_ID: u16 = 0x1AF4;

/// PCI configuration space offsets (32-bit-aligned reads via pci_config_read).
const PCI_BAR0: u8 = 0x10;
const PCI_CAP_PTR: u8 = 0x34;
const PCI_COMMAND_STATUS: u8 = 0x04;

/// PCI capability IDs.
const PCI_CAP_ID_VENDOR: u8 = 0x09;

/// Virtio PCI capability types (virtio 1.2 §4.1.4).
pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

/// Discovered PCI device with virtio capability offsets resolved.
#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,

    /// BAR0 base (masked address).
    pub bar0: u32,
    pub bar0_size: u32,
    /// True if BAR0 is I/O port (not MMIO).
    pub is_io_bar: bool,

    pub common_cfg_offset: u32,
    pub common_cfg_bar: u8,

    pub notify_cfg_offset: u32,
    pub notify_cfg_bar: u8,
    pub notify_off_multiplier: u32,

    pub device_cfg_offset: u32,
    pub device_cfg_bar: u8,

    pub isr_cfg_offset: u32,
    pub isr_cfg_bar: u8,

    /// Physical base of the BAR that holds the virtio cap regions
    /// (typically all four caps share one BAR — `common_cfg_bar`).
    /// 0 if no caps were found.
    pub cap_bar_phys: u64,
    /// Size in bytes of `cap_bar_phys` (page-rounded by mappers).
    pub cap_bar_size: u32,

    /// True if this device speaks modern virtio (1.0+).
    /// Set to true if all four virtio caps were discovered, regardless of
    /// device_id (transitional devices on some QEMU builds expose modern
    /// caps in BAR4 even though they advertise the legacy device_id).
    pub is_modern: bool,
}

/// Find the first virtio PCI device whose `device_id` is in `accepted_device_ids`.
/// Sets `is_modern = true` if the matched id is also in `modern_device_ids`.
pub fn find_virtio_device(
    pci_token: usize,
    accepted_device_ids: &[u16],
    modern_device_ids: &[u16],
) -> Result<PciDevice> {
    let _ = libcluu::debug_print("virtio-core/pci: scanning for virtio device...");

    for bus in 0..8u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let ids = pci::read_ids(pci_token, bus, device, function);
                if let Ok((vendor_id, device_id)) = ids {
                    if vendor_id == VIRTIO_VENDOR_ID
                        && accepted_device_ids.contains(&device_id)
                    {
                        let _ = libcluu::debug_print(&alloc::format!(
                            "virtio-core/pci: found {:04x}:{:04x} at {:02x}:{:02x}.{}",
                            vendor_id,
                            device_id,
                            bus,
                            device,
                            function
                        ));
                        let is_modern = modern_device_ids.contains(&device_id);
                        return init_device(
                            pci_token, bus, device, function, vendor_id, device_id, is_modern,
                        );
                    }
                }
            }
        }
    }

    let _ = libcluu::debug_print("virtio-core/pci: no matching virtio device found");
    Err(Error::NotFound)
}

/// Param-driven variant of `find_virtio_device`.
///
/// If `params[PARAM_DEVICE_PATH] != 0`, the driver was spawned by drivermgr
/// with the device's packed BDF (bus<<16 | dev<<8 | func). We skip the PCI
/// bus scan and validate the named device's vendor/device IDs directly.
/// Otherwise, we delegate to `find_virtio_device` (unchanged self-probe).
pub fn find_virtio_device_with_params(
    pci_token: usize,
    accepted_device_ids: &[u16],
    modern_device_ids: &[u16],
    params: &[u64; 32],
) -> Result<PciDevice> {
    let packed = params[PARAM_DEVICE_PATH];
    if packed == 0 {
        return find_virtio_device(pci_token, accepted_device_ids, modern_device_ids);
    }

    let bus = ((packed >> 16) & 0xFF) as u8;
    let device = ((packed >> 8) & 0xFF) as u8;
    let function = (packed & 0xFF) as u8;

    let _ = libcluu::debug_print(&alloc::format!(
        "virtio-core/pci: init from params BDF={:02x}:{:02x}.{}",
        bus,
        device,
        function
    ));

    let (vendor_id, device_id) = pci::read_ids(pci_token, bus, device, function)?;
    if vendor_id != VIRTIO_VENDOR_ID || !accepted_device_ids.contains(&device_id) {
        let _ = libcluu::debug_print(&alloc::format!(
            "virtio-core/pci: param device {:02x}:{:02x}.{} not a matching virtio device ({:04x}:{:04x})",
            bus,
            device,
            function,
            vendor_id,
            device_id
        ));
        return Err(Error::NotFound);
    }

    let is_modern = modern_device_ids.contains(&device_id);
    init_device(
        pci_token, bus, device, function, vendor_id, device_id, is_modern,
    )
}

/// Resolve the legacy PCI interrupt line (offset 0x3C low byte) for `dev`.
///
/// In param-driven mode (`params[PARAM_DEVICE_PATH] != 0`), drivermgr has
/// already assigned the IRQ and passes it via `params[PARAM_IRQ_LINE]` —
/// we return that directly. Otherwise we fall back to reading PCI config
/// space offset 0x3C (the existing self-probe path).
pub fn get_irq_line(
    pci_token: usize,
    dev: &PciDevice,
    params: &[u64; 32],
) -> Result<usize> {
    if params[PARAM_DEVICE_PATH] != 0 {
        return Ok(params[PARAM_IRQ_LINE] as usize);
    }

    let intr_line_word = pci_config_read(
        pci_token,
        dev.bus,
        dev.device,
        dev.function,
        0x3c,
    )?;
    Ok((intr_line_word & 0xFF) as usize)
}

fn init_device(
    pci_token: usize,
    bus: u8,
    device: u8,
    function: u8,
    vendor_id: u16,
    device_id: u16,
    is_modern: bool,
) -> Result<PciDevice> {
    // Modern-only virtio devices may leave BAR0 unprogrammed (parse_bar
    // returns None for `bar_value == 0`). That's fine — caps live in
    // whichever BAR the capability walk discovers, and `cap_bar_phys` is
    // populated from THAT one below.
    let bar0_raw = pci::config_read_u32(pci_token, bus, device, function, PCI_BAR0)?;
    let (bar0_addr, is_mmio, bar0_size) = match pci::parse_bar(bar0_raw) {
        Some(info) => (
            info.address,
            !info.is_io,
            pci::measure_bar_size(pci_token, bus, device, function, PCI_BAR0, bar0_raw)?,
        ),
        None => (0, true, 0),
    };

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
        cap_bar_phys: 0,
        cap_bar_size: 0,
        is_modern,
    };

    parse_virtio_caps(pci_token, &mut dev)?;

    // Resolve the cap-bearing BAR's physical base and size. Modern virtio
    // typically routes all four caps through one BAR (e.g. BAR4 on QEMU);
    // that BAR is often 64-bit, so we may need to combine the low half
    // (`cap_bar`) with the high half (`cap_bar + 1`).
    let caps_complete = dev.common_cfg_offset != 0
        || dev.notify_cfg_offset != 0
        || dev.isr_cfg_offset != 0
        || dev.device_cfg_offset != 0;
    if caps_complete {
        let cap_bar = dev.common_cfg_bar;
        let bar_reg_offset = PCI_BAR0 + cap_bar * 4;
        let bar_raw = pci::config_read_u32(pci_token, bus, device, function, bar_reg_offset)?;
        if let Some(info) = pci::parse_bar(bar_raw) {
            // 32-bit address bits within the low BAR.
            let low = (bar_raw & 0xFFFF_FFF0) as u64;
            let high = if info.is_64bit {
                let high_off = bar_reg_offset + 4;
                pci::config_read_u32(pci_token, bus, device, function, high_off)? as u64
            } else {
                0
            };
            dev.cap_bar_phys = (high << 32) | low;
            dev.cap_bar_size =
                pci::measure_bar_size(pci_token, bus, device, function, bar_reg_offset, bar_raw)?;
            dev.is_modern = true;
        }
    }

    Ok(dev)
}

fn parse_virtio_caps(pci_token: usize, dev: &mut PciDevice) -> Result<()> {
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

    let cap_ptr_dword = pci_config_read(pci_token, dev.bus, dev.device, dev.function, PCI_CAP_PTR)?;
    let mut offset = (cap_ptr_dword & 0xFF) as u8;

    while offset != 0 && offset != 0xFF {
        let cap_header = pci_config_read(pci_token, dev.bus, dev.device, dev.function, offset)?;
        let cap_id = (cap_header & 0xFF) as u8;
        let next_ptr = ((cap_header >> 8) & 0xFF) as u8;

        if cap_id == PCI_CAP_ID_VENDOR {
            parse_virtio_cap(pci_token, dev, offset)?;
        }

        offset = next_ptr;
    }

    Ok(())
}

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

/// Enable bus mastering and memory/IO space access for a virtio PCI device.
pub fn enable_device(pci_token: usize, dev: &PciDevice) -> Result<()> {
    pci::enable_device(
        pci_token,
        dev.bus,
        dev.device,
        dev.function,
        true, // I/O space
        true, // memory space
        true, // bus master
    )
}
