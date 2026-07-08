use alloc::format;
use libcluu::pci;
use libcluu::{Error, Result};

const XHCI_CLASS_CODE: u32 = 0x0C0330;

#[derive(Debug, Clone)]
pub struct XhciPciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub bar_phys: u64,
    pub bar_size: u32,
    pub irq_line: u8,
}

pub fn find_xhci_device(pci_token: usize) -> Result<XhciPciDevice> {
    for bus in 0..8u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let class = match pci::config_read_u32(pci_token, bus, device, function, 0x08) {
                    Ok(v) => v >> 8,
                    Err(_) => continue,
                };
                if class == XHCI_CLASS_CODE {
                    let (vendor_id, device_id) = pci::read_ids(pci_token, bus, device, function)?;
                    let bar_raw = pci::config_read_u32(pci_token, bus, device, function, 0x10)?;
                    let (bar_phys, bar_size) = if let Some(info) = pci::parse_bar(bar_raw) {
                        let low = (bar_raw & 0xFFFF_FFF0) as u64;
                        let high = if info.is_64bit {
                            pci::config_read_u32(pci_token, bus, device, function, 0x14)? as u64
                        } else {
                            0
                        };
                        let phys = (high << 32) | low;
                        let _ = libcluu::debug_print(&format!(
                            "xhci-core: BAR low=0x{:08x} high=0x{:08x} phys=0x{:x} is64={}",
                            bar_raw, high, phys, info.is_64bit
                        ));
                        pci::enable_device(pci_token, bus, device, function, false, true, true)?;
                        let new_bar_lo = 0xE000_0000u32;
                        pci::config_write_u32(pci_token, bus, device, function, 0x10, new_bar_lo)?;
                        pci::config_write_u32(pci_token, bus, device, function, 0x14, 0)?;
                        let bar_after = pci::config_read_u32(pci_token, bus, device, function, 0x10)?;
                        let high_after = pci::config_read_u32(pci_token, bus, device, function, 0x14)?;
                        let phys_after = ((high_after as u64) << 32) | (bar_after & 0xFFFF_FFF0) as u64;
                        let _ = libcluu::debug_print(&format!(
                            "xhci-core: BAR reprogrammed to 0x{:x}", phys_after
                        ));
                        (phys_after, 0x4000)
                    } else {
                        (0u64, 0u32)
                    };
                    let irq_dword =
                        pci::config_read_u32(pci_token, bus, device, function, 0x3C)?;
                    let irq_line = (irq_dword & 0xFF) as u8;
                    let _ = libcluu::debug_print(&format!(
                        "xhci-core: found xHCI at {:02x}:{:02x}.{} IRQ {}",
                        bus, device, function, irq_line
                    ));
                    return Ok(XhciPciDevice {
                        bus,
                        device,
                        function,
                        vendor_id,
                        device_id,
                        bar_phys,
                        bar_size,
                        irq_line,
                    });
                }
            }
        }
    }
    Err(Error::NotFound)
}
