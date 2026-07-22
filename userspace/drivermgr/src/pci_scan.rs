//! PCI bus scan for drivermgr (D1.2).
//!
//! Walks bus 0..255, device 0..32, function 0..8 via the libcluu PCI config
//! primitives.  For each populated function, builds a `DeviceNode` and
//! inserts it into the shared `DeviceTree`.
//!
//! Empty-bus / empty-slot skipping:
//! - A config read on an unpopulated function returns vendor=0xFFFF.  We
//!   treat that as "no device" and `continue` without touching any further
//!   registers — so an empty bus costs 32 single-register reads (one per
//!   device, function 0), not 32*8*~10 reads.
//! - For an occupied device, we read the header-type byte (offset 0x0C,
//!   low byte).  Bit 7 is the multi-function flag; when it is clear only
//!   function 0 exists and functions 1..7 are skipped entirely.

extern crate alloc;

use alloc::format;

use libcluu::pci;
use libcluu::{debug_print, Result};

use crate::device_tree::{pci_path, DeviceNode, DeviceTree};

const HEADER_TYPE_OFFSET: u8 = 0x0C;
const CLASS_REVISION_OFFSET: u8 = 0x08;
const IRQ_INFO_OFFSET: u8 = 0x3C;
const BAR_BASE_OFFSET: u8 = 0x10;
const MULTIFUNCTION_BIT: u32 = 1 << 7;

/// Scan the PCI bus range and populate `tree` with one `DeviceNode` per
/// populated function.  Returns the count of devices inserted.
pub fn scan(pci_token: usize, tree: &mut DeviceTree) -> Result<usize> {
    let mut count = 0usize;
    for bus in 0u8..=255u8 {
        for device in 0u8..32 {
            let (vendor_id, _device_id) = match pci::read_ids(pci_token, bus, device, 0) {
                Ok(ids) => ids,
                Err(_) => continue,
            };
            // No device at function 0 → skip this slot entirely.
            if vendor_id == 0xFFFF {
                continue;
            }

            let header_type = pci::config_read_u32(pci_token, bus, device, 0, HEADER_TYPE_OFFSET)
                .unwrap_or(0);
            let max_function = if (header_type & MULTIFUNCTION_BIT) != 0 { 8 } else { 1 };

            for function in 0u8..max_function {
                if function != 0 {
                    let (vid, _) = match pci::read_ids(pci_token, bus, device, function) {
                        Ok(ids) => ids,
                        Err(_) => continue,
                    };
                    if vid == 0xFFFF {
                        continue;
                    }
                }
                if let Some(node) = probe_function(pci_token, bus, device, function) {
                    let _ = debug_print(&format!(
                        "drivermgr: found {} vendor={:04x} device={:04x}",
                        node.path, node.vendor_id.unwrap_or(0), node.device_id.unwrap_or(0)
                    ));
                    tree.insert(node.path.clone(), node);
                    count += 1;
                }
            }
        }
        // QEMU typically uses bus 0 only.  We walk the full 0..255 range and
        // rely on the per-slot vendor=0xFFFF short-circuit above to keep the
        // cost bounded at ~32 reads per empty bus.
    }
    let _ = debug_print(&format!("drivermgr: PCI scan complete, {} devices", count));
    Ok(count)
}

fn probe_function(
    pci_token: usize,
    bus: u8,
    device: u8,
    function: u8,
) -> Option<DeviceNode> {
    let (vendor_id, device_id) = pci::read_ids(pci_token, bus, device, function).ok()?;
    if vendor_id == 0xFFFF {
        return None;
    }
    let class_rev = pci::config_read_u32(
        pci_token,
        bus,
        device,
        function,
        CLASS_REVISION_OFFSET,
    )
    .ok()?;
    let irq_info = pci::config_read_u32(
        pci_token,
        bus,
        device,
        function,
        IRQ_INFO_OFFSET,
    )
    .unwrap_or(0);

    let mut node = DeviceNode::new_pci(pci_path(bus, device, function));
    node.vendor_id = Some(vendor_id);
    node.device_id = Some(device_id);
    node.class_code = Some(class_rev >> 8);
    node.bdf = Some((bus, device, function));
    node.irq_line = Some((irq_info & 0xFF) as u8);

    let mut i = 0usize;
    while i < 6 {
        let off = BAR_BASE_OFFSET + (i as u8) * 4;
        let bar_raw = pci::config_read_u32(pci_token, bus, device, function, off).unwrap_or(0);
        if bar_raw == 0 {
            i += 1;
            continue;
        }
        let is_io = (bar_raw & 1) != 0;
        let is_64bit = !is_io && ((bar_raw >> 1) & 0x3) == 2;
        let low = if is_io { bar_raw & 0xFFFF_FFFC } else { bar_raw & 0xFFFF_FFF0 };
        node.bars[i] = Some(low);
        i = if is_64bit { i + 2 } else { i + 1 };
    }

    Some(node)
}
