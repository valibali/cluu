//! Bind rules for driver-to-device matching (D2.5 + D2.6).
//!
//! A `BindRule` describes the conditions under which a driver container
//! should bind to a device. Rules are loaded from `[driver]` sections in
//! container manifests (`/var/images/<name>/manifest.toml`) and matched
//! against the `DeviceTree` populated by PCI/ACPI scan.
//!
//! Matching is observe-only in D2.6: rules are evaluated and results
//! logged, but no drivers are spawned.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use libcluu::toml::{TomlDoc, TomlTable};

use crate::device_tree::{DeviceBus, DeviceNode};

/// A single driver bind rule, derived from a container manifest's
/// `[driver]` section.
#[allow(dead_code)]
// rationale: critical/source_initrd_path/dma consumed by D3 spawn path.
#[derive(Debug, Clone)]
pub struct BindRule {
    pub driver_name: String,
    pub bus: String,
    pub vendor_id: Option<u32>,
    pub device_ids: Vec<u32>,
    pub class_code: Option<u32>,
    pub acpi_hid: Option<String>,
    pub priority: i32,
    pub critical: bool,
    pub source_initrd_path: Option<String>,
    pub dma: bool,
    pub token_slots: Vec<(usize, u32)>,
}

/// A collection of bind rules, sorted by priority (high to low).
#[derive(Debug, Clone, Default)]
pub struct BindRuleTable {
    rules: Vec<BindRule>,
}

impl BindRuleTable {
    pub const fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn add(&mut self, rule: BindRule) {
        self.rules.push(rule);
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    #[allow(dead_code)]
    // rationale: convenience for callers that check empty before iteration.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Sort rules by priority descending (highest priority first).
    /// Stable sort preserves insertion order for equal priorities.
    pub fn sort_by_priority(&mut self) {
        self.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    /// Return an iterator over rules in priority order (must call
    /// `sort_by_priority` first).
    #[allow(dead_code)]
    // rationale: convenience for D3 spawn path and debugging.
    pub fn iter(&self) -> core::slice::Iter<'_, BindRule> {
        self.rules.iter()
    }

    /// Find the highest-priority rule matching `device`.
    /// Returns `Some(&BindRule)` if a match is found, `None` otherwise.
    /// Must be called after `sort_by_priority`.
    pub fn match_device(&self, device: &DeviceNode) -> Option<&BindRule> {
        self.rules.iter().find(|rule| rule_matches(rule, device))
    }
}

fn rule_matches(rule: &BindRule, device: &DeviceNode) -> bool {
    match device.bus {
        DeviceBus::Pci => {
            if rule.bus != "pci" {
                return false;
            }
            if let Some(rule_vid) = rule.vendor_id {
                match device.vendor_id {
                    Some(vid) if vid as u32 == rule_vid => {}
                    _ => return false,
                }
            }
            if !rule.device_ids.is_empty() {
                let did = match device.device_id {
                    Some(d) => d as u32,
                    None => return false,
                };
                if !rule.device_ids.contains(&did) {
                    return false;
                }
            }
            if let Some(rule_class) = rule.class_code {
                match device.class_code {
                    Some(c) if c == rule_class => {}
                    _ => return false,
                }
            }
            true
        }
        DeviceBus::Acpi => {
            if rule.bus != "acpi" {
                return false;
            }
            match (&rule.acpi_hid, &device.acpi_hid) {
                (Some(rule_hid), Some(dev_hid)) => rule_hid == dev_hid,
                _ => false,
            }
        }
    }
}

/// Parse a hex or decimal string to u32. Accepts "0x1af4", "0x1AF4",
/// "4380", etc. Returns None on parse failure.
fn parse_u32(s: &str) -> Option<u32> {
    let trimmed = s.trim();
    if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        u32::from_str_radix(trimmed, 10).ok()
    }
}

/// Parse a boolean string ("true"/"false") to bool.
fn parse_bool(s: &str) -> bool {
    s.trim() == "true"
}

/// Build a `BindRule` from a parsed manifest document.
///
/// The manifest must have a `[driver]` section (checked by caller) and
/// may contain `[[driver.bind]]`, `[[driver.hardware]]`,
/// `[[driver.lifecycle]]`, `[[driver.source]]`, `[[driver.envelope]]`
/// array-of-tables sub-sections.
///
/// Returns `None` if the `[[driver.bind]]` section is missing or has no
/// bus field — a bind rule without a bus is meaningless.
pub fn build_rule_from_manifest(driver_name: &str, doc: &TomlDoc) -> Option<BindRule> {
    let binds = doc.array_tables("driver.bind");
    let bind: &TomlTable = binds.first()?;

    let bus = String::from(bind.get_str("bus")?);

    let vendor_id = bind.get_str("vendor").and_then(parse_u32);
    let device_ids = bind
        .get_array("devices")
        .map(|arr| arr.iter().filter_map(|s| parse_u32(s)).collect())
        .unwrap_or_default();
    let class_code = bind.get_str("class").and_then(parse_u32);
    let acpi_hid = bind.get_str("hid").map(String::from);

    let mut priority: i32 = 100;
    let mut critical = false;
    let mut dma = false;
    let mut source_initrd_path: Option<String> = None;

    for entry in doc.array_tables("driver.envelope") {
        if let Some(p) = entry.get_str("priority").and_then(parse_u32) {
            priority = p as i32;
        }
    }

    for entry in doc.array_tables("driver.lifecycle") {
        if let Some(c) = entry.get_str("critical") {
            critical = parse_bool(c);
        }
    }

    for entry in doc.array_tables("driver.hardware") {
        if let Some(d) = entry.get_str("dma") {
            dma = parse_bool(d);
        }
    }

    for entry in doc.array_tables("driver.source") {
        if let Some(path) = entry.get_str("initrd_path") {
            source_initrd_path = Some(String::from(path));
        }
    }

    let mut token_slots: Vec<(usize, u32)> = Vec::new();
    for entry in doc.array_tables("driver.tokens") {
        if let (Some(slot_str), Some(rights_str)) = (entry.get_str("slot"), entry.get_str("rights")) {
            let slot = parse_u32(slot_str).unwrap_or(0) as usize;
            let rights = parse_u32(rights_str).unwrap_or(0);
            token_slots.push((slot, rights));
        }
    }

    Some(BindRule {
        driver_name: String::from(driver_name),
        bus,
        vendor_id,
        device_ids,
        class_code,
        acpi_hid,
        priority,
        critical,
        source_initrd_path,
        dma,
        token_slots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_tree::{DeviceNode, pci_path};

    fn pci_node(path: &str, vid: u16, did: u16, class: u32) -> DeviceNode {
        let mut node = DeviceNode::new_pci(String::from(path));
        node.vendor_id = Some(vid);
        node.device_id = Some(did);
        node.class_code = Some(class);
        node
    }

    fn acpi_node(hid: &str) -> DeviceNode {
        DeviceNode::new_acpi(
            crate::device_tree::acpi_path(hid),
            String::from(hid),
        )
    }

    #[test]
    fn match_pci_vendor_device() {
        let rule = BindRule {
            driver_name: "virtio-blk".into(),
            bus: "pci".into(),
            vendor_id: Some(0x1af4),
            device_ids: vec![0x1001, 0x1042],
            class_code: None,
            acpi_hid: None,
            priority: 100,
            critical: false,
            source_initrd_path: None,
            dma: false,
            token_slots: Vec::new(),
        };
        let table = BindRuleTable { rules: vec![rule] };
        let dev = pci_node("/pci/00:04.0", 0x1af4, 0x1042, 0x010000);
        assert!(table.match_device(&dev).is_some());
    }

    #[test]
    fn no_match_wrong_device_id() {
        let rule = BindRule {
            driver_name: "virtio-blk".into(),
            bus: "pci".into(),
            vendor_id: Some(0x1af4),
            device_ids: vec![0x1001, 0x1042],
            class_code: None,
            acpi_hid: None,
            priority: 100,
            critical: false,
            source_initrd_path: None,
            dma: false,
            token_slots: Vec::new(),
        };
        let table = BindRuleTable { rules: vec![rule] };
        let dev = pci_node("/pci/00:05.0", 0x1af4, 0x9999, 0x010000);
        assert!(table.match_device(&dev).is_none());
    }

    #[test]
    fn match_pci_class_only() {
        let rule = BindRule {
            driver_name: "usb-input".into(),
            bus: "pci".into(),
            vendor_id: None,
            device_ids: vec![],
            class_code: Some(0x0c0320),
            acpi_hid: None,
            priority: 100,
            critical: false,
            source_initrd_path: None,
            dma: false,
            token_slots: Vec::new(),
        };
        let table = BindRuleTable { rules: vec![rule] };
        let dev = pci_node("/pci/00:06.0", 0x1234, 0x5678, 0x0c0320);
        assert!(table.match_device(&dev).is_some());
    }

    #[test]
    fn match_acpi_hid() {
        let rule = BindRule {
            driver_name: "kbd".into(),
            bus: "acpi".into(),
            vendor_id: None,
            device_ids: vec![],
            class_code: None,
            acpi_hid: Some("PNP0303".into()),
            priority: 100,
            critical: false,
            source_initrd_path: None,
            dma: false,
            token_slots: Vec::new(),
        };
        let table = BindRuleTable { rules: vec![rule] };
        let dev = acpi_node("PNP0303");
        assert!(table.match_device(&dev).is_some());
    }

    #[test]
    fn highest_priority_wins() {
        let low = BindRule {
            driver_name: "low".into(),
            bus: "pci".into(),
            vendor_id: Some(0x1af4),
            device_ids: vec![],
            class_code: None,
            acpi_hid: None,
            priority: 50,
            critical: false,
            source_initrd_path: None,
            dma: false,
            token_slots: Vec::new(),
        };
        let high = BindRule {
            driver_name: "high".into(),
            bus: "pci".into(),
            vendor_id: Some(0x1af4),
            device_ids: vec![],
            class_code: None,
            acpi_hid: None,
            priority: 200,
            critical: false,
            source_initrd_path: None,
            dma: false,
            token_slots: Vec::new(),
        };
        let mut table = BindRuleTable::new();
        table.add(low);
        table.add(high);
        table.sort_by_priority();
        let dev = pci_node("/pci/00:04.0", 0x1af4, 0x1042, 0x010000);
        let m = table.match_device(&dev).unwrap();
        assert_eq!(m.driver_name, "high");
    }

    #[test]
    fn sort_by_priority_descending() {
        let mut table = BindRuleTable::new();
        table.add(BindRule {
            driver_name: "a".into(),
            bus: "pci".into(),
            vendor_id: None, device_ids: vec![], class_code: None,
            acpi_hid: None, priority: 50, critical: false,
            source_initrd_path: None, dma: false, token_slots: Vec::new(),
        });
        table.add(BindRule {
            driver_name: "b".into(),
            bus: "pci".into(),
            vendor_id: None, device_ids: vec![], class_code: None,
            acpi_hid: None, priority: 200, critical: false,
            source_initrd_path: None, dma: false, token_slots: Vec::new(),
        });
        table.sort_by_priority();
        assert_eq!(table.iter().next().unwrap().driver_name, "b");
    }

    #[test]
    fn parse_u32_hex_and_decimal() {
        assert_eq!(parse_u32("0x1af4"), Some(0x1af4));
        assert_eq!(parse_u32("0X1AF4"), Some(0x1af4));
        assert_eq!(parse_u32("4380"), Some(4380));
        assert_eq!(parse_u32("0x10000"), Some(0x10000));
        assert_eq!(parse_u32("garbage"), None);
    }

    #[test]
    fn build_rule_from_virtio_blk_manifest() {
        let toml = r#"
[container]
name = "virtio-blk"

[driver]

[[driver.bind]]
bus = "pci"
vendor = 0x1af4
devices = [0x1001, 0x1042]
class = 0x10000

[[driver.hardware]]
dma = true

[[driver.lifecycle]]
critical = true

[[driver.source]]
initrd_path = "sys/virtio-blk.elf"
"#;
        let doc = libcluu::toml::parse(toml).expect("parse");
        let rule = build_rule_from_manifest("virtio-blk", &doc).expect("rule");
        assert_eq!(rule.driver_name, "virtio-blk");
        assert_eq!(rule.bus, "pci");
        assert_eq!(rule.vendor_id, Some(0x1af4));
        assert_eq!(rule.device_ids, vec![0x1001, 0x1042]);
        assert_eq!(rule.class_code, Some(0x10000));
        assert!(rule.dma);
        assert!(rule.critical);
        assert_eq!(rule.source_initrd_path.as_deref(), Some("sys/virtio-blk.elf"));
        assert_eq!(rule.priority, 100);
    }

    #[test]
    fn build_rule_with_envelope_priority() {
        let toml = r#"
[driver]

[[driver.bind]]
bus = "pci"
vendor = 0x1af4

[[driver.envelope]]
priority = 180
"#;
        let doc = libcluu::toml::parse(toml).expect("parse");
        let rule = build_rule_from_manifest("test", &doc).expect("rule");
        assert_eq!(rule.priority, 180);
    }

    #[test]
    fn build_rule_acpi_hid() {
        let toml = r#"
[driver]

[[driver.bind]]
bus = "acpi"
hid = "PNP0303"
"#;
        let doc = libcluu::toml::parse(toml).expect("parse");
        let rule = build_rule_from_manifest("kbd", &doc).expect("rule");
        assert_eq!(rule.bus, "acpi");
        assert_eq!(rule.acpi_hid.as_deref(), Some("PNP0303"));
    }

    #[test]
    fn build_rule_without_bind_returns_none() {
        let toml = r#"
[driver]

[[driver.hardware]]
dma = true
"#;
        let doc = libcluu::toml::parse(toml).expect("parse");
        assert!(build_rule_from_manifest("test", &doc).is_none());
    }

    #[test]
    fn pci_path_format() {
        assert_eq!(pci_path(0, 4, 0), "/pci/00:04.0");
    }
}
