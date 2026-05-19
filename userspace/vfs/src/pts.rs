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
    /// Session this PTS belongs to (None = global namespace, visible to all).
    pub session_id: Option<u32>,
}

// ─── PtsOverlay ───────────────────────────────────────────────────────────────

/// Per-session + global PTS storage.
///
/// Entries are placed in either `global` (when `session_id` is `None`) or
/// `by_session[sid]` (when `session_id` is `Some`).  View derivation in
/// `view.rs` uses this to substitute `/dev/pts` with the session-private
/// subset.
pub struct PtsOverlay {
    /// Session id → pts id → PtsEntry (session-private terminals).
    pub by_session: BTreeMap<u32, BTreeMap<u32, PtsEntry>>,
    /// Pts id → PtsEntry (globally visible terminals, e.g. /dev/tty1..3).
    pub global: BTreeMap<u32, PtsEntry>,
    /// Next id allocator per namespace (`None` key = global).
    next_id: BTreeMap<Option<u32>, u32>,
}

impl PtsOverlay {
    /// Get a mutable reference to the correct map for a session.
    fn map_for_session_mut(
        &mut self,
        session_id: Option<u32>,
    ) -> &mut BTreeMap<u32, PtsEntry> {
        match session_id {
            None => &mut self.global,
            Some(sid) => self.by_session.entry(sid).or_default(),
        }
    }

    /// Get a reference to the correct map for a session.
    fn map_for_session(
        &self,
        session_id: Option<u32>,
    ) -> Option<&BTreeMap<u32, PtsEntry>> {
        match session_id {
            None => Some(&self.global),
            Some(sid) => self.by_session.get(&sid),
        }
    }

    /// Look up an entry across all maps (global + every session).
    fn find_entry(&self, id: u32) -> Option<&PtsEntry> {
        if let Some(e) = self.global.get(&id) {
            return Some(e);
        }
        for map in self.by_session.values() {
            if let Some(e) = map.get(&id) {
                return Some(e);
            }
        }
        None
    }

    /// Look up a mutable entry across all maps.
    fn find_entry_mut(&mut self, id: u32) -> Option<&mut PtsEntry> {
        if let Some(e) = self.global.get_mut(&id) {
            return Some(e);
        }
        for map in self.by_session.values_mut() {
            if let Some(e) = map.get_mut(&id) {
                return Some(e);
            }
        }
        None
    }
}

// ─── PtsRegistry ──────────────────────────────────────────────────────────────

/// Registry of all live PTS entries.  Owned by `VfsServer`.
///
/// Entries are split between `global` (sessionless) and per-session maps
/// inside `PtsOverlay`.  Lookups must scan both.
pub struct PtsRegistry {
    overlay: PtsOverlay,
}

impl PtsRegistry {
    pub fn new() -> Self {
        Self {
            overlay: PtsOverlay {
                by_session: BTreeMap::new(),
                global: BTreeMap::new(),
                next_id: BTreeMap::new(),
            },
        }
    }

    /// Allocate a new PTS slot in the global namespace (sessionless).
    ///
    /// Used by the legacy `PTS_REGISTER_LABEL` (0x70) path and by the new
    /// `VFS_REGISTER_PTS_LABEL` path when `session_id` is `None`.
    ///
    /// `owner_tid` — authenticated sender tid of the registrant.
    /// `notify_endpoint` — endpoint token to receive `PTS_CLOSED_LABEL`.
    pub fn register(&mut self, owner_tid: usize, notify_endpoint: usize) -> Option<u32> {
        self.register_in_session(None, owner_tid, notify_endpoint)
    }

    /// Allocate a new PTS slot in a specific session.
    ///
    /// When `session_id` is `None`, the entry lands in the global map.
    /// When `Some(sid)`, the entry lands in `by_session[sid]` and is
    /// invisible outside that session's view.
    pub fn register_in_session(
        &mut self,
        session_id: Option<u32>,
        owner_tid: usize,
        notify_endpoint: usize,
    ) -> Option<u32> {
        // Snapshot the next_id cursor before the mutable borrow from
        // map_for_session_mut takes &mut self.overlay.
        let mut next_id = *self
            .overlay
            .next_id
            .get(&session_id)
            .unwrap_or(&0);

        let map = self.overlay.map_for_session_mut(session_id);
        if map.len() >= MAX_PTS_SLOTS {
            return None;
        }
        let start = next_id;
        loop {
            let id = next_id;
            next_id = next_id.wrapping_add(1);
            if !map.contains_key(&id) {
                map.insert(
                    id,
                    PtsEntry {
                        id,
                        owner_tid,
                        notify_endpoint,
                        refcount: 0,
                        session_id,
                    },
                );
                // Write back the updated cursor.
                self.overlay.next_id.insert(session_id, next_id);
                return Some(id);
            }
            if next_id == start {
                return None; // Full despite len check.
            }
        }
    }

    // --- Delegating helpers that just forward to PtsOverlay ---

    /// Look up an entry across all maps.
    fn find_entry(&self, id: u32) -> Option<&PtsEntry> {
        self.overlay.find_entry(id)
    }

    /// Look up a mutable entry across all maps.
    fn find_entry_mut(&mut self, id: u32) -> Option<&mut PtsEntry> {
        self.overlay.find_entry_mut(id)
    }

    /// Remove a PTS entry from whichever map it lives in.  No-op if not found.
    pub fn unregister(&mut self, id: u32) {
        if self.overlay.global.remove(&id).is_some() {
            return;
        }
        for map in self.overlay.by_session.values_mut() {
            if map.remove(&id).is_some() {
                return;
            }
        }
    }

    /// Increment the open-fd refcount for `id`.
    /// Returns the new refcount, or `None` if the id is unknown.
    pub fn inc_ref(&mut self, id: u32) -> Option<u32> {
        self.find_entry_mut(id).map(|e| {
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
        self.find_entry_mut(id).map(|e| {
            if e.refcount > 0 {
                e.refcount -= 1;
            }
            (e.refcount, e.notify_endpoint)
        })
    }

    /// Return the `notify_endpoint` for `id`, or `None` if unknown.
    pub fn notify_endpoint(&self, id: u32) -> Option<usize> {
        self.find_entry(id).map(|e| e.notify_endpoint)
    }

    /// Return the `owner_tid` for `id`, or `None` if unknown.
    pub fn owner_tid(&self, id: u32) -> Option<usize> {
        self.find_entry(id).map(|e| e.owner_tid)
    }

    /// Return `true` if `id` is currently registered.
    pub fn contains(&self, id: u32) -> bool {
        self.find_entry(id).is_some()
    }

    /// Iterate over all currently registered ids (in sorted order, global first).
    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        let global_ids = self.overlay.global.keys().copied();
        let session_ids = self
            .overlay
            .by_session
            .values()
            .flat_map(|m| m.keys().copied());
        global_ids.chain(session_ids)
    }

    /// Return all ids visible within a given session's view.
    ///
    /// `None` session → only global entries.
    /// `Some(sid)` → global entries + entries from `by_session[sid]`.
    pub fn ids_for_session(&self, session_id: Option<u32>) -> impl Iterator<Item = u32> + '_ {
        let global = self.overlay.global.keys().copied();
        let session = match session_id {
            None => None,
            Some(sid) => self.overlay.by_session.get(&sid),
        };
        // Collect session ids into a Vec to avoid borrow issues with chain.
        let session_vec: Vec<u32> = session
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default();
        global.chain(session_vec.into_iter())
    }

    /// Check whether `id` is visible within a given session's view.
    pub fn contains_for_session(&self, id: u32, session_id: Option<u32>) -> bool {
        if self.overlay.global.contains_key(&id) {
            return true;
        }
        match session_id {
            None => false,
            Some(sid) => self
                .overlay
                .by_session
                .get(&sid)
                .map(|m| m.contains_key(&id))
                .unwrap_or(false),
        }
    }

    /// Full overlay access for constructing a session-private PtsBackend.
    pub fn overlay(&self) -> &PtsOverlay {
        &self.overlay
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
        let mut notifiable = Vec::new();

        // Scan global map.
        let global_to_remove: Vec<u32> = self
            .overlay
            .global
            .iter()
            .filter(|(_, e)| e.owner_tid == dead_tid)
            .map(|(id, _)| *id)
            .collect();
        for id in global_to_remove {
            if let Some(entry) = self.overlay.global.remove(&id) {
                if entry.refcount > 0 {
                    notifiable.push((id, entry.notify_endpoint));
                }
            }
        }

        // Scan per-session maps.
        for map in self.overlay.by_session.values_mut() {
            let to_remove: Vec<u32> = map
                .iter()
                .filter(|(_, e)| e.owner_tid == dead_tid)
                .map(|(id, _)| *id)
                .collect();
            for id in to_remove {
                if let Some(entry) = map.remove(&id) {
                    if entry.refcount > 0 {
                        notifiable.push((id, entry.notify_endpoint));
                    }
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
///
/// Two variants:
/// - `Session(None)` — the global `/dev/pts/` mount, visible to all.
/// - `Session(Some(sid))` — a session-private overlay derived during view
///   narrowing; only shows pts entries registered under that session plus
///   global entries.
pub struct PtsBackend {
    /// Raw pointer to the VfsServer's PtsRegistry.
    /// SAFETY: valid for entire lifetime of the VFS service.
    registry: *const PtsRegistry,
    /// If `Some`, only show entries visible to this session.
    session_id: Option<u32>,
}

impl PtsBackend {
    /// Construct a global backend.
    ///
    /// `registry_ptr` must point to a `PtsRegistry` that outlives all calls to
    /// methods on this backend.
    pub fn new(registry_ptr: *const PtsRegistry) -> Self {
        Self {
            registry: registry_ptr,
            session_id: None,
        }
    }

    /// Construct a session-private backend.
    pub fn for_session(registry_ptr: *const PtsRegistry, session_id: u32) -> Self {
        Self {
            registry: registry_ptr,
            session_id: Some(session_id),
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
    /// Returns a `VirtualFile` placeholder when the id is registered
    /// and visible in this backend's session scope.
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

        if !self.reg().contains_for_session(id, self.session_id) {
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
    /// For the empty/root path, returns one `DirEntry` per registered id
    /// that is visible in this backend's session scope.
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
                .ids_for_session(self.session_id)
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
        if self.reg().contains_for_session(id, self.session_id) {
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
