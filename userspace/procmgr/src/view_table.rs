//! Procmgr-owned ViewObject table.
//!
//! A `ViewObject` represents the VFS view a process sees. Each carries
//! a parent pointer (for derive chains), a list of mounts, and a refcount
//! tracking how many tokens reference it. Spec 1 §8.

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use cluu_proto::spawn::SpawnError;

pub type ViewObjectId = u32;

/// One mount entry inside a view.
#[derive(Clone, Debug)]
pub struct MountEntry {
    pub path: String,
    pub rights: MountRights,
    /// Backend reference (memfs id, ext2 path, devfs marker, etc.).
    /// Stored as opaque bytes; interpretation is procmgr's mount-policy
    /// concern, not this module's.
    pub backend: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MountRights {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl MountRights {
    pub fn is_subset_of(self, other: Self) -> bool {
        (!self.read || other.read)
            && (!self.write || other.write)
            && (!self.execute || other.execute)
    }
}

#[derive(Clone, Debug)]
pub struct ViewObject {
    pub id: ViewObjectId,
    pub parent: Option<ViewObjectId>,
    pub mounts: Vec<MountEntry>,
    pub refcount: u32,
}

pub struct ViewTable {
    inner: Mutex<ViewTableInner>,
}

struct ViewTableInner {
    next_id: ViewObjectId,
    entries: BTreeMap<ViewObjectId, ViewObject>,
}

impl ViewTable {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(ViewTableInner {
                next_id: 1,
                entries: BTreeMap::new(),
            }),
        }
    }

    pub fn insert(&self, parent: Option<ViewObjectId>, mounts: Vec<MountEntry>) -> ViewObjectId {
        let mut g = self.inner.lock();
        let id = g.next_id;
        g.next_id = g.next_id.wrapping_add(1);
        g.entries.insert(id, ViewObject { id, parent, mounts, refcount: 1 });
        id
    }

    pub fn inc_ref(&self, id: ViewObjectId) -> Result<(), SpawnError> {
        let mut g = self.inner.lock();
        let e = g.entries.get_mut(&id).ok_or(SpawnError::ViewDeriveDenied)?;
        e.refcount = e.refcount.saturating_add(1);
        Ok(())
    }

    pub fn dec_ref(&self, id: ViewObjectId) {
        let mut g = self.inner.lock();
        if let Some(e) = g.entries.get_mut(&id) {
            e.refcount = e.refcount.saturating_sub(1);
            if e.refcount == 0 {
                g.entries.remove(&id);
            }
        }
    }

    pub fn snapshot(&self, id: ViewObjectId) -> Option<ViewObject> {
        self.inner.lock().entries.get(&id).cloned()
    }
}

pub static VIEW_TABLE: ViewTable = ViewTable::new();

/// Monotone-decrease check: every entry in `child_mounts` must be a
/// narrower-or-equal subset of some entry in `parent_mounts` (same path
/// prefix, rights ≤ parent's).
pub fn verify_monotone(child_mounts: &[MountEntry], parent_mounts: &[MountEntry])
    -> Result<(), SpawnError>
{
    for cm in child_mounts {
        let matched = parent_mounts.iter().find(|pm| cm.path.starts_with(pm.path.as_str()));
        match matched {
            None => return Err(SpawnError::ViewDeriveDenied),
            Some(pm) => {
                if !cm.rights.is_subset_of(pm.rights) {
                    return Err(SpawnError::ViewDeriveDenied);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn mk_mount(path: &str, r: bool, w: bool) -> MountEntry {
        MountEntry {
            path: String::from(path),
            rights: MountRights { read: r, write: w, execute: false },
            backend: Vec::new(),
        }
    }

    #[test]
    fn child_narrower_rights_accepted() {
        let parent = vec![mk_mount("/home", true, true)];
        let child = vec![mk_mount("/home", true, false)];
        assert!(verify_monotone(&child, &parent).is_ok());
    }

    #[test]
    fn child_wider_rights_rejected() {
        let parent = vec![mk_mount("/home", true, false)];
        let child = vec![mk_mount("/home", true, true)];
        assert!(verify_monotone(&child, &parent).is_err());
    }

    #[test]
    fn child_unknown_path_rejected() {
        let parent = vec![mk_mount("/home", true, true)];
        let child = vec![mk_mount("/etc", true, false)];
        assert!(verify_monotone(&child, &parent).is_err());
    }

    #[test]
    fn child_subpath_accepted() {
        let parent = vec![mk_mount("/home", true, true)];
        let child = vec![mk_mount("/home/dave", true, true)];
        assert!(verify_monotone(&child, &parent).is_ok());
    }
}