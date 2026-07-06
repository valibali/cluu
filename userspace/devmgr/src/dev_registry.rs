//! Device registry — the data model for devmgr.
//!
//! Stores registered devices and answers visibility queries. Contains no
//! IPC or syscall code so the logic is unit-testable in isolation.
//!
//! SOLID:
//! - SRP: only stores and queries device metadata.
//! - ISP: exposes focused methods (`register_block`, `register_char`,
//!   `get`, `list_for_envelope`, `list_all`) — callers depend only on
//!   what they use.
//! - DIP: depends only on `DeviceEntry` / `DeviceClass` abstractions.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::device::{DeviceClass, DeviceEntry, DeviceId};

#[derive(Clone, Debug)]
pub struct VisibleDevice {
    pub path: String,
    pub device_id: DeviceId,
    pub class: DeviceClass,
    pub rights: u32,
}

pub struct DevRegistry {
    devices: BTreeMap<DeviceId, DeviceEntry>,
    next_id: DeviceId,
}

impl DevRegistry {
    pub fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            next_id: 0,
        }
    }

    pub fn register_block(
        &mut self,
        device_id: DeviceId,
        path: String,
        driver_endpoint: usize,
        root_token: usize,
        total_sectors: u64,
    ) {
        self.devices.insert(
            device_id,
            DeviceEntry::new_block(path, driver_endpoint, root_token, total_sectors),
        );
    }

    pub fn register_char(
        &mut self,
        class: DeviceClass,
        path: String,
        driver_endpoint: usize,
        root_token: usize,
    ) -> DeviceId {
        let id = self.next_id;
        self.next_id += 1;
        self.devices
            .insert(id, DeviceEntry::new_char(class, path, driver_endpoint, root_token));
        id
    }

    pub fn get(&self, id: DeviceId) -> Option<&DeviceEntry> {
        self.devices.get(&id)
    }

    pub fn get_mut(&mut self, id: DeviceId) -> Option<&mut DeviceEntry> {
        self.devices.get_mut(&id)
    }

    pub fn find_by_path(&self, path: &str) -> Option<DeviceId> {
        self.devices
            .iter()
            .find(|(_, e)| e.path == path)
            .map(|(id, _)| *id)
    }

    pub fn list_all(&self) -> Vec<VisibleDevice> {
        self.devices
            .iter()
            .map(|(id, e)| VisibleDevice {
                path: e.path.clone(),
                device_id: *id,
                class: e.class,
                rights: 0xFFFF_FFFF,
            })
            .collect()
    }

    /// Return devices visible to an envelope.
    ///
    /// - `is_root`: root session (§6 godmode) → all devices.
    /// - `cluufile_devices`: paths declared in the Cluufile `DEVICE` lines.
    ///   Non-root sessions see only devices whose path appears in this list.
    pub fn list_for_envelope(
        &self,
        is_root: bool,
        cluufile_devices: &[String],
    ) -> Vec<VisibleDevice> {
        if is_root {
            return self.list_all();
        }
        self.devices
            .iter()
            .filter(|(_, e)| cluufile_devices.iter().any(|d| d == &e.path))
            .map(|(id, e)| VisibleDevice {
                path: e.path.clone(),
                device_id: *id,
                class: e.class,
                rights: 0xFFFF_FFFF,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> DevRegistry {
        DevRegistry::new()
    }

    #[test]
    fn register_block_stores_entry() {
        let mut r = reg();
        r.register_block(0, "/dev/disk/0".into(), 0x100, 0x200, 1024);
        let e = r.get(0).unwrap();
        assert_eq!(e.class, DeviceClass::Block);
        assert_eq!(e.path, "/dev/disk/0");
        assert_eq!(e.total_sectors, 1024);
    }

    #[test]
    fn register_char_assigns_incremental_id() {
        let mut r = reg();
        let id1 = r.register_char(DeviceClass::Input, "/dev/input/kbd".into(), 0x10, 0x20);
        let id2 = r.register_char(DeviceClass::Input, "/dev/input/mouse".into(), 0x30, 0x40);
        assert_ne!(id1, id2);
        assert_eq!(r.get(id1).unwrap().path, "/dev/input/kbd");
        assert_eq!(r.get(id2).unwrap().path, "/dev/input/mouse");
    }

    #[test]
    fn find_by_path_returns_id() {
        let mut r = reg();
        r.register_block(0, "/dev/disk/0".into(), 0, 0, 0);
        assert_eq!(r.find_by_path("/dev/disk/0"), Some(0));
        assert_eq!(r.find_by_path("/dev/missing"), None);
    }

    #[test]
    fn list_all_returns_every_device() {
        let mut r = reg();
        r.register_block(0, "/dev/disk/0".into(), 0, 0, 0);
        r.register_char(DeviceClass::Input, "/dev/input/kbd".into(), 0, 0);
        assert_eq!(r.list_all().len(), 2);
    }

    #[test]
    fn list_for_envelope_root_sees_all() {
        let mut r = reg();
        r.register_block(0, "/dev/disk/0".into(), 0, 0, 0);
        r.register_char(DeviceClass::Input, "/dev/input/mouse".into(), 0, 0);
        let visible = r.list_for_envelope(true, &[]);
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn list_for_envelope_non_root_sees_only_declared() {
        let mut r = reg();
        r.register_block(0, "/dev/disk/0".into(), 0, 0, 0);
        r.register_char(DeviceClass::Input, "/dev/input/mouse".into(), 0, 0);
        r.register_char(DeviceClass::Input, "/dev/input/kbd".into(), 0, 0);
        let decls = vec!["/dev/input/mouse".to_string()];
        let visible = r.list_for_envelope(false, &decls);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].path, "/dev/input/mouse");
    }

    #[test]
    fn list_for_envelope_non_root_empty_decls_sees_nothing() {
        let mut r = reg();
        r.register_block(0, "/dev/disk/0".into(), 0, 0, 0);
        let visible = r.list_for_envelope(false, &[]);
        assert!(visible.is_empty());
    }
}
