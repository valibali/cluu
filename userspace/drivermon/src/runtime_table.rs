//! Runtime table domain types for drivermon.
//!
//! Pure data — no IPC, no syscalls. Separated from `main.rs` so the types
//! can be referenced without pulling in the orchestration logic.
//!
//! Phase D1 skeleton: struct definitions only. Exit-notify (D1.6),
//! REGISTER/RESPAWN/REBIND IPC (D3.3), and restart/fault handling (D4)
//! will populate and mutate this table.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

/// Restart policy for a supervised driver.
#[allow(dead_code)]
// rationale: variants consumed by D4 restart/fault handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartPolicy {
    /// Always restart on exit (including clean exit 0).
    Always,
    /// Never restart — let the driver stay dead.
    Never,
    /// Restart only on fault (non-zero exit or kernel fault IPC).
    OnFault,
}

/// Lifecycle state of a supervised driver entry.
#[allow(dead_code)]
// rationale: Restarting/Failed constructed by D4 restart/fault handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverState {
    /// Driver is running and bound to its device.
    Bound,
    /// Driver is in the process of being restarted.
    Restarting,
    /// Driver has exhausted its restart budget and no fallback is available.
    Failed,
}

/// A supervised driver runtime entry, keyed by PID.
///
/// One entry per driver process spawned by drivermgr. When a fallback
/// replaces a failed driver, the old entry is replaced with a new one
/// for the new PID.
#[derive(Clone, Debug)]
#[allow(dead_code)]
// rationale: fields read by D4 exit-notify and restart-budget logic.
pub struct RuntimeEntry {
    /// PID of the supervised driver process.
    pub pid: u32,
    /// Device path in the drivermgr device tree (e.g. `/pci/00:04.0`).
    pub device_path: String,
    /// Driver image name (e.g. `virtio-blk`).
    pub driver_image: String,
    /// Restart policy for this driver.
    pub policy: RestartPolicy,
    /// Fallback driver image, if configured.
    pub fallback: Option<String>,
    /// Number of restarts since boot (or since last window reset).
    pub restart_count: u32,
    /// Monotonic timestamp of the last restart, in milliseconds.
    pub last_restart_ms: u64,
    /// Maximum restarts allowed within the time window.
    pub max_restarts: u32,
    /// Restart budget window length, in seconds.
    pub window_secs: u64,
    /// Fallback images already tried (cycle detection).
    pub visited_fallbacks: Vec<String>,
    /// Current lifecycle state.
    pub state: DriverState,
}

impl RuntimeEntry {
    /// Create a new runtime entry for a freshly spawned driver.
    #[allow(dead_code)]
    // rationale: convenience constructor for REGISTER handler (D3.3).
    pub fn new(
        pid: u32,
        device_path: String,
        driver_image: String,
        policy: RestartPolicy,
        fallback: Option<String>,
    ) -> Self {
        Self {
            pid,
            device_path,
            driver_image,
            policy,
            fallback,
            restart_count: 0,
            last_restart_ms: 0,
            max_restarts: 4,
            window_secs: 30,
            visited_fallbacks: Vec::new(),
            state: DriverState::Bound,
        }
    }
}

/// Runtime table keyed by driver PID. Newtype wrapper so supervision
/// operations (`register`/`respawn`/`rebind`) can live on the table.
#[allow(dead_code)]
// rationale: consumed by D3.3 REGISTER handler and D4 restart logic.
#[derive(Default)]
pub struct DriverRuntimeTable {
    by_pid: BTreeMap<u32, RuntimeEntry>,
}

impl DriverRuntimeTable {
    pub const fn new() -> Self {
        Self { by_pid: BTreeMap::new() }
    }

    #[allow(dead_code)]
    // rationale: BTreeMap-style accessors used by tests; D4 restart logic will consume them.
    pub fn len(&self) -> usize {
        self.by_pid.len()
    }

    #[allow(dead_code)]
    // rationale: see `len`.
    pub fn is_empty(&self) -> bool {
        self.by_pid.is_empty()
    }

    #[allow(dead_code)]
    // rationale: see `len`.
    pub fn contains_key(&self, pid: &u32) -> bool {
        self.by_pid.contains_key(pid)
    }

    #[allow(dead_code)]
    // rationale: see `len`.
    pub fn get(&self, pid: &u32) -> Option<&RuntimeEntry> {
        self.by_pid.get(pid)
    }

    fn insert(&mut self, pid: u32, entry: RuntimeEntry) {
        self.by_pid.insert(pid, entry);
    }

    fn values_mut(&mut self) -> alloc::collections::btree_map::ValuesMut<'_, u32, RuntimeEntry> {
        self.by_pid.values_mut()
    }

    /// Register a freshly spawned driver. Overwrites any existing entry
    /// for `pid`.
    pub fn register(
        &mut self,
        pid: u32,
        device_path: String,
        driver_image: String,
        policy: RestartPolicy,
        fallback: Option<String>,
    ) {
        self.insert(pid, RuntimeEntry::new(pid, device_path, driver_image, policy, fallback));
    }

    /// Find the entry owning `device_path`, increment its `restart_count`,
    /// and return a reference to it. `None` if no entry owns the path.
    pub fn respawn(&mut self, device_path: &str) -> Option<&RuntimeEntry> {
        let entry = self
            .values_mut()
            .find(|e| e.device_path == device_path)?;
        entry.restart_count = entry.restart_count.saturating_add(1);
        Some(entry)
    }

    /// Check if the entry for `pid` has exceeded its restart budget.
    /// Returns true if restart is allowed, false if budget exceeded.
    /// A window reset (time since last_restart > window) always allows
    /// a restart and the caller should call `reset_restart_window` to
    /// clear the counter.
    pub fn check_restart_budget(&self, pid: &u32, now_ms: u64) -> bool {
        match self.get(pid) {
            Some(entry) => {
                if now_ms.saturating_sub(entry.last_restart_ms) > entry.window_secs * 1000 {
                    return true;
                }
                entry.restart_count < entry.max_restarts
            }
            None => false,
        }
    }

    /// Reset the restart counter and update last_restart_ms for `pid`.
    /// Called after a successful restart trigger (within budget or
    /// window reset).
    pub fn bump_restart(&mut self, pid: &u32, now_ms: u64) {
        if let Some(entry) = self.by_pid.get_mut(pid) {
            if now_ms.saturating_sub(entry.last_restart_ms) > entry.window_secs * 1000 {
                entry.restart_count = 0;
            }
            entry.restart_count = entry.restart_count.saturating_add(1);
            entry.last_restart_ms = now_ms;
            entry.state = DriverState::Restarting;
        }
    }

    /// Mark `pid` as Failed (no restart, no fallback available).
    pub fn mark_failed(&mut self, pid: &u32) {
        if let Some(entry) = self.by_pid.get_mut(pid) {
            entry.state = DriverState::Failed;
        }
    }

    /// Mark `pid` as Restarting (fallback or respawn in progress).
    pub fn mark_restarting(&mut self, pid: &u32) {
        if let Some(entry) = self.by_pid.get_mut(pid) {
            entry.state = DriverState::Restarting;
        }
    }

    /// Remove the entry for `pid` (driver exited cleanly, no restart).
    pub fn remove(&mut self, pid: &u32) {
        self.by_pid.remove(pid);
    }

    /// Record a visited fallback for `pid` (cycle detection).
    pub fn add_visited_fallback(&mut self, pid: &u32, image: &str) {
        if let Some(entry) = self.by_pid.get_mut(pid) {
            entry.visited_fallbacks.push(String::from(image));
        }
    }

    /// Replace the `driver_image` of the entry owning `device_path`;
    /// returns the old PID. PID is preserved so exit-notify still
    /// correlates with the rebinding entry. `None` if no entry owns
    /// the path.
    pub fn rebind(&mut self, device_path: &str, new_driver_image: String) -> Option<u32> {
        let entry = self
            .values_mut()
            .find(|e| e.device_path == device_path)?;
        let old_pid = entry.pid;
        entry.driver_image = new_driver_image;
        Some(old_pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_entry_starts_bound() {
        let entry = RuntimeEntry::new(
            42,
            "/pci/00:04.0".into(),
            "virtio-blk".into(),
            RestartPolicy::Always,
            None,
        );
        assert_eq!(entry.state, DriverState::Bound);
        assert_eq!(entry.restart_count, 0);
        assert_eq!(entry.max_restarts, 4);
        assert_eq!(entry.window_secs, 30);
        assert!(entry.visited_fallbacks.is_empty());
    }

    #[test]
    fn table_keys_by_pid() {
        let mut table = DriverRuntimeTable::new();
        let entry = RuntimeEntry::new(
            7,
            "/pci/00:05.0".into(),
            "usb-input".into(),
            RestartPolicy::OnFault,
            Some("test-usb-fallback".into()),
        );
        table.insert(entry.pid, entry);
        assert!(table.contains_key(&7));
        assert_eq!(table.get(&7).unwrap().driver_image, "usb-input");
    }

    #[test]
    fn register_inserts_entry() {
        let mut table = DriverRuntimeTable::new();
        table.register(
            11,
            "/pci/00:04.0".into(),
            "virtio-blk".into(),
            RestartPolicy::Always,
            None,
        );
        let entry = table.get(&11).expect("registered entry");
        assert_eq!(entry.device_path, "/pci/00:04.0");
        assert_eq!(entry.driver_image, "virtio-blk");
        assert_eq!(entry.policy, RestartPolicy::Always);
        assert_eq!(entry.state, DriverState::Bound);
        assert_eq!(entry.restart_count, 0);
    }

    #[test]
    fn register_with_fallback_stores_fallback() {
        let mut table = DriverRuntimeTable::new();
        table.register(
            12,
            "/pci/00:05.0".into(),
            "usb-input".into(),
            RestartPolicy::OnFault,
            Some("test-usb-fallback".into()),
        );
        let entry = table.get(&12).expect("registered entry");
        assert_eq!(entry.fallback.as_deref(), Some("test-usb-fallback"));
    }

    #[test]
    fn register_overwrites_existing_pid() {
        let mut table = DriverRuntimeTable::new();
        table.register(20, "/pci/00:04.0".into(), "virtio-blk".into(), RestartPolicy::Always, None);
        table.register(20, "/pci/00:04.0".into(), "virtio-blk-v2".into(), RestartPolicy::Always, None);
        assert_eq!(table.get(&20).unwrap().driver_image, "virtio-blk-v2");
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn respawn_increments_restart_count() {
        let mut table = DriverRuntimeTable::new();
        table.register(30, "/pci/00:04.0".into(), "virtio-blk".into(), RestartPolicy::Always, None);
        assert_eq!(table.get(&30).unwrap().restart_count, 0);

        let entry = table.respawn("/pci/00:04.0").expect("found entry");
        assert_eq!(entry.restart_count, 1);
        assert_eq!(table.get(&30).unwrap().restart_count, 1);

        let entry = table.respawn("/pci/00:04.0").expect("found entry");
        assert_eq!(entry.restart_count, 2);
    }

    #[test]
    fn respawn_unknown_device_returns_none() {
        let mut table = DriverRuntimeTable::new();
        table.register(30, "/pci/00:04.0".into(), "virtio-blk".into(), RestartPolicy::Always, None);
        assert!(table.respawn("/pci/00:09.9").is_none());
        assert_eq!(table.get(&30).unwrap().restart_count, 0);
    }

    #[test]
    fn rebind_replaces_driver_image_and_returns_old_pid() {
        let mut table = DriverRuntimeTable::new();
        table.register(40, "/pci/00:05.0".into(), "usb-input".into(), RestartPolicy::OnFault, None);
        let old_pid = table.rebind("/pci/00:05.0", "test-usb-fallback".into()).expect("found");
        assert_eq!(old_pid, 40);
        assert_eq!(table.get(&40).unwrap().driver_image, "test-usb-fallback");
    }

    #[test]
    fn rebind_unknown_device_returns_none() {
        let mut table = DriverRuntimeTable::new();
        table.register(40, "/pci/00:05.0".into(), "usb-input".into(), RestartPolicy::OnFault, None);
        assert!(table.rebind("/pci/00:09.9", "x".into()).is_none());
        assert_eq!(table.get(&40).unwrap().driver_image, "usb-input");
    }
}
