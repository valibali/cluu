//! Device tree domain types for drivermgr.
//!
//! Pure data — no IPC, no syscalls. Separated from `main.rs` so the types
//! can be referenced without pulling in the orchestration logic.
//!
//! Phase D1 skeleton: struct definitions only. PCI/ACPI scan (D1.2/D1.3)
//! and /proc/devices rendering (D1.4) will populate and query this tree.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

/// Bus on which a device was discovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceBus {
    Pci,
    Acpi,
}

/// Lifecycle state of a device node.
#[allow(dead_code)]
// rationale: Bound/Degraded/Failed constructed by D4.5 state transitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceState {
    /// No driver bound.
    Unbound,
    /// A driver is bound and healthy.
    Bound,
    /// Driver is degraded (restart in progress or fallback active).
    Degraded,
    /// Driver failed and no fallback available.
    Failed,
}

/// A discovered device, keyed by path in the device tree.
///
/// Path conventions:
/// - PCI: `/pci/XX:YY.Z` (bus:device.function)
/// - ACPI: `/acpi/<HID>` (e.g. `/acpi/PNP0303`)
#[derive(Clone, Debug)]
pub struct DeviceNode {
    /// Canonical path (e.g. `/pci/00:04.0`, `/acpi/PNP0303`).
    pub path: String,
    /// Bus where the device was discovered.
    pub bus: DeviceBus,
    /// PCI vendor ID, if PCI.
    pub vendor_id: Option<u16>,
    /// PCI device ID, if PCI.
    pub device_id: Option<u16>,
    /// PCI class code (base/subclass/prog-if), if PCI.
    pub class_code: Option<u32>,
    /// PCI bus/device/function tuple, if PCI.
    pub bdf: Option<(u8, u8, u8)>,
    /// PCI base address registers (6 slots).
    pub bars: [Option<u32>; 6],
    /// IRQ line, if assigned.
    pub irq_line: Option<u8>,
    /// ACPI PNP hardware ID (e.g. `PNP0303`), if ACPI.
    pub acpi_hid: Option<String>,
    /// I/O port ranges claimed by the device.
    pub io_ports: Vec<u16>,
    /// Current lifecycle state.
    pub state: DeviceState,
}

impl DeviceNode {
    /// Create a minimal PCI device node with unknown state.
    #[allow(dead_code)]
    // rationale: convenience constructor for PCI scan (D1.2).
    pub fn new_pci(path: String) -> Self {
        Self {
            path,
            bus: DeviceBus::Pci,
            vendor_id: None,
            device_id: None,
            class_code: None,
            bdf: None,
            bars: [None; 6],
            irq_line: None,
            acpi_hid: None,
            io_ports: Vec::new(),
            state: DeviceState::Unbound,
        }
    }

    /// Create a minimal ACPI device node with unknown state.
    #[allow(dead_code)]
    // rationale: convenience constructor for ACPI scan (D6.2).
    pub fn new_acpi(path: String, hid: String) -> Self {
        Self {
            path,
            bus: DeviceBus::Acpi,
            vendor_id: None,
            device_id: None,
            class_code: None,
            bdf: None,
            bars: [None; 6],
            irq_line: None,
            acpi_hid: Some(hid),
            io_ports: Vec::new(),
            state: DeviceState::Unbound,
        }
    }
}

/// The device tree, keyed by canonical device path.
pub type DeviceTree = BTreeMap<String, DeviceNode>;

/// Render the device tree as text for `/proc/devices`.
///
/// One line per device, sorted by path (BTreeMap ordering). Format:
/// ```text
/// /pci/00:04.0  vendor=1af4 device=1042 class=010000 state=unbound
/// /acpi/PNP0303  hid=PNP0303 state=unbound
/// ```
#[allow(dead_code)]
// rationale: consumed by /proc/devices procfs backend (D1.4).
pub fn query_all(tree: &DeviceTree) -> String {
    let mut out = String::new();
    for node in tree.values() {
        out.push_str(&node.path);
        match node.bus {
            DeviceBus::Pci => {
                if let Some(v) = node.vendor_id {
                    out.push_str(&format!(" vendor=0x{:04x}", v));
                }
                if let Some(d) = node.device_id {
                    out.push_str(&format!(" device=0x{:04x}", d));
                }
                if let Some(c) = node.class_code {
                    out.push_str(&format!(" class=0x{:06x}", c));
                }
            }
            DeviceBus::Acpi => {
                if let Some(hid) = &node.acpi_hid {
                    out.push_str(&format!(" hid={}", hid));
                }
            }
        }
        out.push_str(" state=");
        out.push_str(match node.state {
            DeviceState::Unbound => "unbound",
            DeviceState::Bound => "bound",
            DeviceState::Degraded => "degraded",
            DeviceState::Failed => "failed",
        });
        out.push('\n');
    }
    out
}

/// Render a single device node as multi-line text for
/// `/proc/devices/pci/XX:YY.Z`.
#[allow(dead_code)]
// rationale: consumed by /proc/devices per-device procfs (D1.4).
pub fn query_device(node: &DeviceNode) -> String {
    let mut out = String::new();
    out.push_str(&format!("path={}\n", node.path));
    out.push_str(&format!(
        "bus={}\n",
        match node.bus {
            DeviceBus::Pci => "pci",
            DeviceBus::Acpi => "acpi",
        }
    ));
    if let Some(v) = node.vendor_id {
        out.push_str(&format!("vendor_id=0x{:04x}\n", v));
    }
    if let Some(d) = node.device_id {
        out.push_str(&format!("device_id=0x{:04x}\n", d));
    }
    if let Some(c) = node.class_code {
        out.push_str(&format!("class_code=0x{:06x}\n", c));
    }
    if let Some((bus, dev, func)) = node.bdf {
        out.push_str(&format!("bdf={:02x}:{:02x}.{}\n", bus, dev, func));
    }
    for (i, bar) in node.bars.iter().enumerate() {
        if let Some(addr) = bar {
            out.push_str(&format!("bar{}=0x{:08x}\n", i, addr));
        }
    }
    if let Some(irq) = node.irq_line {
        out.push_str(&format!("irq_line={}\n", irq));
    }
    if let Some(hid) = &node.acpi_hid {
        out.push_str(&format!("acpi_hid={}\n", hid));
    }
    if !node.io_ports.is_empty() {
        out.push_str("io_ports=");
        for (i, port) in node.io_ports.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("0x{:04x}", port));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "state={}\n",
        match node.state {
            DeviceState::Unbound => "unbound",
            DeviceState::Bound => "bound",
            DeviceState::Degraded => "degraded",
            DeviceState::Failed => "failed",
        }
    ));
    out
}

/// Convert a `(bus, device, function)` tuple to the canonical PCI path
/// `/pci/XX:YY.Z`.
#[allow(dead_code)]
// rationale: convenience for PCI scan (D1.2).
pub fn pci_path(bus: u8, device: u8, function: u8) -> String {
    format!("/pci/{:02x}:{:02x}.{}", bus, device, function)
}

/// Convert an ACPI PNP HID to the canonical ACPI path `/acpi/<HID>`.
#[allow(dead_code)]
// rationale: convenience for ACPI scan (D6.2).
pub fn acpi_path(hid: &str) -> String {
    format!("/acpi/{}", hid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_all_renders_pci_device() {
        let mut tree = DeviceTree::new();
        let mut node = DeviceNode::new_pci(pci_path(0, 4, 0));
        node.vendor_id = Some(0x1af4);
        node.device_id = Some(0x1042);
        node.class_code = Some(0x010000);
        tree.insert(node.path.clone(), node);
        let out = query_all(&tree);
        assert!(out.contains("/pci/00:04.0 vendor=0x1af4 device=0x1042 class=0x010000 state=unbound"));
    }

    #[test]
    fn query_all_renders_acpi_device() {
        let mut tree = DeviceTree::new();
        let node = DeviceNode::new_acpi(acpi_path("PNP0303"), "PNP0303".to_string());
        tree.insert(node.path.clone(), node);
        let out = query_all(&tree);
        assert!(out.contains("/acpi/PNP0303 hid=PNP0303 state=unbound"));
    }
}
