//! PTS (pseudo-terminal slave) virtual namespace — `/dev/pts/<id>`.
//!
//! # Architecture
//!
//! `PtsRegistry` is a plain BTreeMap-backed table owned directly by
//! `VfsServer`.  It is **not** a `MountBackend` itself; the thin `PtsBackend`
//! wrapper implements `MountBackend` for path routing under `/dev/pts/`.
//!
//! VFS is single-threaded.  `PtsBackend` therefore holds a raw pointer to the
//! `PtsRegistry` that lives inside `VfsServer`.  The pointer is valid for the
//! entire lifetime of the server (the registry is in a heap-allocated
//! `VfsServer` that is never freed).
//!
//! ## Registration life-cycle
//!
//! 1. `cluuterm` sends `PTS_REGISTER_LABEL` to VFS.
//!    `VfsServer` calls `PtsRegistry::register(owner_tid, notify_endpoint)`.
//!    Reply: assigned id.
//!
//! 2. `VfsServer::handle_open` opens `/dev/pts/<id>` via `PtsBackend::open`,
//!    then calls `PtsRegistry::inc_ref(id)` on success.
//!
//! 3. `VfsServer::handle_close` calls `PtsRegistry::dec_ref(id)`.
//!    When `new_refcount == 0` it fire-and-forgets `PTS_CLOSED_LABEL` to
//!    `notify_endpoint`.
//!
//! 4. `cluuterm` sends `PTS_UNREGISTER_LABEL` to explicitly release the slot
//!    (idempotent). Only the original registrant (matched by `owner_tid`) may
//!    unregister.
//!
//! ## owner_tid vs. notify_endpoint
//!
//! In the CLUU IPC model, delivering a message to a thread requires an
//! *endpoint token*, not a raw tid.  Therefore each entry stores both:
//! - `owner_tid`: authenticated sender tid (from IPC envelope) for ownership
//!   validation on `PTS_UNREGISTER_LABEL`.
//! - `notify_endpoint`: endpoint token passed by the registrant in `words[0]`;
//!   VFS uses this to fire-and-forget `PTS_CLOSED_LABEL`.
//!
//! ## MountBackend surface (Task 13 scope)
//!
//! - `readdir("/dev/pts")` — lists currently registered ids as `DirEntry`s.
//! - `open("/dev/pts/<id>")` — validates that the id is registered; returns a
//!   `VirtualFile` placeholder.  VfsServer increments the refcount after the
//!   open succeeds.
//! - `read` / `write` — stubbed (`Error::NotImplemented`) until Task 15 wires
//!   the owner-routed IPC path.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fd_table::{OpenFile, PtsFile};
use crate::mount::{DirEntry, DirEntryStat, MountBackend};
use libcluu::{Error, Result};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Maximum number of simultaneously registered pseudo-terminal slaves.
pub const MAX_PTS_SLOTS: usize = 32;

// ─── PtsEntry ─────────────────────────────────────────────────────────────────

/// Metadata for a single registered PTS slot.
pub struct PtsEntry {
    /// Allocated id (matches the key in the BTreeMap for convenience).
    pub id: u32,
    /// Authenticated sender tid of the PTS_REGISTER_LABEL request.
    /// Used to validate PTS_UNREGISTER_LABEL ownership.
    pub owner_tid: usize,
    /// Endpoint token provided by the registrant.  VFS sends PTS_CLOSED_LABEL
    /// to this endpoint when the last open fd is closed.
    pub notify_endpoint: usize,
    /// Number of currently open VFS file-descriptors pointing at this PTS.
    pub refcount: u32,
}

// ─── PtsRegistry ──────────────────────────────────────────────────────────────

/// Registry of all live PTS entries.  Owned by `VfsServer`.
pub struct PtsRegistry {
    entries: BTreeMap<u32, PtsEntry>,
    next_id: u32,
}

impl PtsRegistry {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            next_id: 0,
        }
    }

    /// Allocate a new PTS slot.  Returns the assigned id, or `None` when the
    /// pool is exhausted (`MAX_PTS_SLOTS` simultaneous registrations).
    ///
    /// `owner_tid` — authenticated sender tid of the registrant.
    /// `notify_endpoint` — endpoint token to receive `PTS_CLOSED_LABEL`.
    pub fn register(&mut self, owner_tid: usize, notify_endpoint: usize) -> Option<u32> {
        if self.entries.len() >= MAX_PTS_SLOTS {
            return None;
        }
        // Find the next free id (wrapping scan).
        let start = self.next_id;
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            if !self.entries.contains_key(&id) {
                self.entries.insert(
                    id,
                    PtsEntry {
                        id,
                        owner_tid,
                        notify_endpoint,
                        refcount: 0,
                    },
                );
                return Some(id);
            }
            if self.next_id == start {
                return None; // Full despite len check (shouldn't happen).
            }
        }
    }

    /// Remove a PTS entry.  No-op if the id is not present.
    pub fn unregister(&mut self, id: u32) {
        self.entries.remove(&id);
    }

    /// Increment the open-fd refcount for `id`.
    /// Returns the new refcount, or `None` if the id is unknown.
    pub fn inc_ref(&mut self, id: u32) -> Option<u32> {
        self.entries.get_mut(&id).map(|e| {
            e.refcount = e.refcount.saturating_add(1);
            e.refcount
        })
    }

    /// Decrement the open-fd refcount for `id`.
    ///
    /// Returns `(new_refcount, notify_endpoint)` so the caller can
    /// fire-and-forget `PTS_CLOSED_LABEL` when `new_refcount == 0`.
    /// Returns `None` if the id is unknown.
    pub fn dec_ref(&mut self, id: u32) -> Option<(u32, usize)> {
        self.entries.get_mut(&id).map(|e| {
            if e.refcount > 0 {
                e.refcount -= 1;
            }
            (e.refcount, e.notify_endpoint)
        })
    }

    /// Return the `notify_endpoint` for `id`, or `None` if unknown.
    pub fn notify_endpoint(&self, id: u32) -> Option<usize> {
        self.entries.get(&id).map(|e| e.notify_endpoint)
    }

    /// Return the `owner_tid` for `id`, or `None` if unknown.
    pub fn owner_tid(&self, id: u32) -> Option<usize> {
        self.entries.get(&id).map(|e| e.owner_tid)
    }

    /// Return `true` if `id` is currently registered.
    pub fn contains(&self, id: u32) -> bool {
        self.entries.contains_key(&id)
    }

    /// Iterate over all currently registered ids (in sorted order).
    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.entries.keys().copied()
    }

    /// Remove every PTS entry whose `owner_tid` matches `dead_tid`.
    ///
    /// Returns a `Vec<(id, notify_endpoint)>` for entries that had
    /// `refcount > 0` when evicted — caller fires `PTS_CLOSED_LABEL` for each.
    ///
    /// Called by `VfsServer` when procmgr notifies VFS of a process exit.
    /// (Owner-death cleanup is **stubbed** in Task 13; this method is the hook
    /// point Task 14+ will call once the procmgr→VFS death notification is
    /// wired.)
    pub fn evict_owner(&mut self, dead_tid: usize) -> Vec<(u32, usize)> {
        let to_remove: Vec<u32> = self
            .entries
            .iter()
            .filter(|(_, e)| e.owner_tid == dead_tid)
            .map(|(id, _)| *id)
            .collect();

        let mut notifiable = Vec::new();
        for id in to_remove {
            if let Some(entry) = self.entries.remove(&id) {
                if entry.refcount > 0 {
                    notifiable.push((id, entry.notify_endpoint));
                }
            }
        }
        notifiable
    }
}

impl Default for PtsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── PtsBackend ───────────────────────────────────────────────────────────────

/// VFS mount backend for `/dev/pts`.
///
/// Holds a raw pointer to the `PtsRegistry` owned by `VfsServer`.  Safe
/// because VFS is single-threaded and the registry outlives the backend (both
/// live inside the same heap-allocated `VfsServer` that is never freed).
pub struct PtsBackend {
    /// Raw pointer to the VfsServer's PtsRegistry.
    /// SAFETY: valid for entire lifetime of the VFS service.
    registry: *const PtsRegistry,
}

impl PtsBackend {
    /// Construct a new backend.
    ///
    /// `registry_ptr` must point to a `PtsRegistry` that outlives all calls to
    /// methods on this backend.
    pub fn new(registry_ptr: *const PtsRegistry) -> Self {
        Self {
            registry: registry_ptr,
        }
    }

    fn reg(&self) -> &PtsRegistry {
        // SAFETY: single-threaded VFS; pointer is valid for the process lifetime.
        unsafe { &*self.registry }
    }
}

// SAFETY: VFS is single-threaded; PtsBackend is never sent across threads.
unsafe impl Send for PtsBackend {}
unsafe impl Sync for PtsBackend {}

impl MountBackend for PtsBackend {
    fn name(&self) -> &'static str {
        "ptsfs"
    }

    /// Open `/dev/pts/<id>`.
    ///
    /// Returns a `VirtualFile` placeholder when the id is registered.
    /// `VfsServer::handle_open` calls `PtsRegistry::inc_ref(id)` after a
    /// successful open.
    ///
    /// Read/write forwarding to the owner terminal is stubbed until Task 15.
    fn open(&self, rel_path: &str, full_path: &str, _caller_tid: usize) -> Result<OpenFile> {
        let rel = rel_path.trim_start_matches('/');

        if rel.is_empty() {
            return Err(Error::InvalidArgument);
        }

        let id: u32 = rel.parse().map_err(|_| Error::NotFound)?;

        if !self.reg().contains(id) {
            return Err(Error::NotFound);
        }

        // Return a Pts handle.  VfsServer increments the refcount after the
        // open succeeds.  Actual read/write forwarding arrives in Task 15.
        Ok(OpenFile::Pts(PtsFile {
            pts_id: id,
            path: String::from(full_path),
        }))
    }

    /// List entries under `/dev/pts`.
    ///
    /// For the empty/root path, returns one `DirEntry` per registered id.
    fn readdir(&self, rel_path: &str, _caller_tid: usize) -> Result<Vec<DirEntry>> {
        let rel = rel_path.trim_start_matches('/');

        let chr_stat = DirEntryStat {
            size: 0,
            mode: 0o020600u32, // S_IFCHR | rw-------
            mtime: 0,
            nlink: 1,
            uid: 0,
            gid: 5, // traditional 'tty' group
            blocks: 0,
        };

        if rel.is_empty() {
            let reg = self.reg();
            let entries = reg
                .ids()
                .map(|id| DirEntry {
                    name: u32_to_decimal(id),
                    is_dir: false,
                    stat: chr_stat,
                })
                .collect();
            return Ok(entries);
        }

        // Individual device node: not a directory.
        let id: u32 = rel.parse().map_err(|_| Error::NotFound)?;
        if self.reg().contains(id) {
            Ok(alloc::vec![])
        } else {
            Err(Error::NotFound)
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Convert `n` to a decimal `String` without `format!`.
fn u32_to_decimal(mut n: u32) -> String {
    if n == 0 {
        return String::from("0");
    }
    let mut buf = [0u8; 10];
    let mut i = 0usize;
    while n > 0 {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    let mut s = String::with_capacity(i);
    while i > 0 {
        i -= 1;
        s.push(buf[i] as char);
    }
    s
}
