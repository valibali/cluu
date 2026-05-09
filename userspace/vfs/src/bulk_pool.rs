//! Per-client shared-frame pool for bulk transport.
//!
//! A `BulkPool` owns a contiguous virtual range pre-mapped in the server's
//! address space, partitioned into fixed-size slots. On `setup` it picks a
//! free slot, `space_grant`s its pages into the client's target_base, and
//! caches the session keyed by client_id. Subsequent ops (e.g. write blob
//! into source_base, reply with len) read the session via `get`.
//!
//! Designed for promotion to `libcluu` once a second service (e.g. net,
//! gpu) needs the same pattern. Ring transport (SPSC + header) is a
//! distinct concern layered on top; this module deliberately stays raw.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use libcluu::syscall::space_grant;
use libcluu::{Error, Result, PAGE_SIZE};

/// Per-client session: a slice of the pool granted to the client at
/// `target_base` in their address space (`target_space`).
#[derive(Clone, Copy)]
pub struct BulkSession {
    /// Server-side virtual base of the slot.
    pub source_base: usize,
    /// Client-side virtual base where the slot was granted.
    pub target_base: usize,
    /// Client's address-space token used at grant time.
    pub target_space: usize,
    /// Slot size in bytes.
    pub bytes: usize,
    /// Pool slot index (for free-list bookkeeping).
    pub slot: usize,
}

/// Server-owned pool of fixed-size shared frames.
pub struct BulkPool {
    pool_base: usize,
    slot_bytes: usize,
    free_slots: Vec<usize>,
    sessions: BTreeMap<usize, BulkSession>,
}

impl BulkPool {
    /// Create a new pool over an already-mapped virtual range.
    /// Caller must ensure `[pool_base, pool_base + slot_bytes * slot_count)`
    /// is mapped RW in the server's address space before any `setup` call.
    pub fn new(pool_base: usize, slot_bytes: usize, slot_count: usize) -> Self {
        let mut free_slots = Vec::with_capacity(slot_count);
        for slot in 0..slot_count {
            free_slots.push(slot);
        }
        free_slots.reverse();
        Self {
            pool_base,
            slot_bytes,
            free_slots,
            sessions: BTreeMap::new(),
        }
    }

    /// Look up an existing session by client id. Cheap; no locking.
    pub fn get(&self, client_id: usize) -> Option<BulkSession> {
        self.sessions.get(&client_id).copied()
    }

    /// Establish (or refresh) a session for `client_id`. If a session
    /// already exists, returns it iff `target_base` and `target_space`
    /// still match — otherwise rejects with `Busy` (a client must not
    /// re-target an existing slot without releasing first).
    ///
    /// On a fresh setup, allocates a slot and `space_grant`s every page
    /// from the slot's source_base to the client's target_base.
    pub fn setup(
        &mut self,
        client_id: usize,
        target_base: usize,
        target_space: usize,
        server_space_token: usize,
    ) -> Result<BulkSession> {
        if target_space == 0 || !target_base.is_multiple_of(PAGE_SIZE) {
            return Err(Error::InvalidArgument);
        }

        if let Some(existing) = self.sessions.get(&client_id).copied() {
            if existing.target_base != target_base || existing.target_space != target_space {
                return Err(Error::Busy);
            }
            return Ok(existing);
        }

        let slot = self.free_slots.pop().ok_or(Error::Busy)?;
        let session = BulkSession {
            source_base: self.pool_base + slot * self.slot_bytes,
            target_base,
            target_space,
            bytes: self.slot_bytes,
            slot,
        };

        let pages = session.bytes.div_ceil(PAGE_SIZE);
        for page_idx in 0..pages {
            let src = session.source_base + page_idx * PAGE_SIZE;
            let dst = session.target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(server_space_token, session.target_space, src, dst, 0x02)
            {
                self.free_slots.push(slot);
                return Err(err);
            }
        }

        self.sessions.insert(client_id, session);
        Ok(session)
    }

    /// Release a client's session and return its slot to the free list.
    /// Idempotent. Currently only invoked on client teardown — bookkeeping
    /// only, no kernel-side unmap (client owns its target_base mapping).
    #[allow(dead_code)]
    pub fn release(&mut self, client_id: usize) {
        if let Some(session) = self.sessions.remove(&client_id) {
            self.free_slots.push(session.slot);
        }
    }
}
