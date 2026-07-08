extern crate alloc;

use alloc::vec::Vec;
use libcluu::pci;
use libcluu::Result;

#[derive(Debug, Clone)]
pub struct PciDeviceInfo {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u32,
    pub irq_pin: u8,
    pub irq_line: u8,
    pub bars: [BarInfo; 6],
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BarInfo {
    pub index: u8,
    pub addr: u64,
    pub size: u32,
    pub is_64bit: bool,
    pub is_io: bool,
}

pub fn enumerate(pci_token: usize) -> Result<Vec<PciDeviceInfo>> {
    let mut devs = Vec::new();
    for bus in 0..8u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let (vendor_id, device_id) = match pci::read_ids(pci_token, bus, device, function) {
                    Ok(ids) => ids,
                    Err(_) => continue,
                };
                if vendor_id == 0xFFFF {
                    continue;
                }
                let class_rev = pci::config_read_u32(pci_token, bus, device, function, 0x08)?;
                let irq_info = pci::config_read_u32(pci_token, bus, device, function, 0x3C)?;
                let bars = read_all_bars(pci_token, bus, device, function)?;
                devs.push(PciDeviceInfo {
                    bus,
                    device,
                    function,
                    vendor_id,
                    device_id,
                    class_code: class_rev >> 8,
                    irq_pin: ((irq_info >> 8) & 0xFF) as u8,
                    irq_line: (irq_info & 0xFF) as u8,
                    bars,
                });
            }
        }
    }
    Ok(devs)
}

fn read_all_bars(pci_token: usize, bus: u8, dev: u8, fnc: u8) -> Result<[BarInfo; 6]> {
    let mut bars = [BarInfo::default(); 6];
    let mut i = 0;
    while i < 6 {
        let bar_offset = 0x10 + i as u8 * 4;
        let bar_raw = pci::config_read_u32(pci_token, bus, dev, fnc, bar_offset)?;
        if bar_raw == 0 {
            i += 1;
            continue;
        }
        let is_io = (bar_raw & 1) != 0;
        let is_64bit = !is_io && ((bar_raw >> 1) & 0x3) == 2;
        let low = if is_io { bar_raw & 0xFFFF_FFFC } else { bar_raw & 0xFFFF_FFF0 } as u64;
        let (addr, next_idx) = if is_64bit {
            let high = pci::config_read_u32(pci_token, bus, dev, fnc, bar_offset + 4)? as u64;
            ((high << 32) | low, i + 2)
        } else {
            (low, i + 1)
        };
        bars[i] = BarInfo {
            index: i as u8,
            addr,
            size: 0,
            is_64bit,
            is_io,
        };
        i = next_idx;
    }
    Ok(bars)
}

pub fn enable(pci_token: usize, dev: &PciDeviceInfo) -> Result<()> {
    pci::enable_device(
        pci_token,
        dev.bus,
        dev.device,
        dev.function,
        true,
        true,
        true,
    )
}

pub fn find_by_class<'a>(devs: &'a [PciDeviceInfo], class_mask: u32, class_match: u32) -> Option<&'a PciDeviceInfo> {
    devs.iter().find(|d| (d.class_code & class_mask) == class_match)
}

pub fn find_by_vendor<'a>(devs: &'a [PciDeviceInfo], vendor: u16, device_ids: &[u16]) -> Option<&'a PciDeviceInfo> {
    devs.iter().find(|d| d.vendor_id == vendor && device_ids.contains(&d.device_id))
}
