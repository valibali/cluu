//! Frame Registry — Capability-based physical frame accounting
//!
//! Tracks allocated physical frames with ownership and mapping counts.
//! Enables frame tokens, grants between address spaces, and cleanup on free.

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::token::scope::{AddressSpaceId, FrameId};

/// Metadata for a tracked physical frame
pub struct FrameEntry {
    /// Physical address of the 4K frame
    pub phys_addr: u64,
    /// Which address space allocated this frame
    pub owner_space: AddressSpaceId,
    /// Number of address spaces currently mapping this frame
    pub map_count: u32,
    /// If true, free the frame back to PMM when `map_count` drops to 0.
    /// False for user-held frame tokens (FrameAllocate) where userspace
    /// owns the lifetime and must call FrameFree explicitly.
    /// True for entries created implicitly by `invoke_space_grant` to
    /// track shared mappings across address spaces.
    pub auto_free: bool,
    /// Number of contiguous 4 KiB pages in this allocation.
    /// Always a power of two (rounded up by buddy allocator).
    /// 1 for single-page allocations (the common case).
    pub page_count: u32,
}

/// Forward map: FrameId → FrameEntry
static FRAME_REGISTRY: Mutex<BTreeMap<FrameId, FrameEntry>> = Mutex::new(BTreeMap::new());

/// Reverse map: physical address → FrameId (for SpaceUnmap lookups)
static PHYS_TO_FRAME: Mutex<BTreeMap<u64, FrameId>> = Mutex::new(BTreeMap::new());

/// Monotonic frame ID counter
static NEXT_FRAME_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRegistryError {
    NotFound,
    StillMapped,
}

/// Allocate a new physical frame and register it.
///
/// Calls `pmm::alloc_frame()` internally. Returns `(FrameId, phys_addr)`.
pub fn alloc_frame(owner: AddressSpaceId) -> Option<(FrameId, u64)> {
    let phys = crate::mm::pmm::alloc_frame_tagged("registry_alloc")?;
    // Phase 2.5: retype as UserData for the registry's owner. LOUD on error.
    if let Err(e) = crate::mm::frame_table::retype_to_user(phys, owner) {
        klibcluu::error("frame_registry::alloc_frame: retype_to_user failed");
        klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", phys);
        klibcluu::log_dec(klibcluu::LogLevel::Error, "  owner=", owner.as_u64());
        let _ = e; // retype failure is non-fatal for registry alloc
    }
    let id = FrameId::new(NEXT_FRAME_ID.fetch_add(1, Ordering::SeqCst));

    let entry = FrameEntry {
        phys_addr: phys,
        owner_space: owner,
        map_count: 0,
        auto_free: false,
        page_count: 1,
    };

    FRAME_REGISTRY.lock().insert(id, entry);
    PHYS_TO_FRAME.lock().insert(phys, id);

    Some((id, phys))
}

/// Register (or reuse) a frame as the backing of a shared grant mapping.
///
/// If the frame is not yet tracked, creates an entry with `map_count = 2`
/// (source mapping + new target mapping) and `auto_free = true`. On
/// subsequent grants of the same frame, just increments `map_count` by 1
/// (one new target mapping).
///
/// Used by `invoke_space_grant` to track shared mappings so that
/// `teardown_user_pages` does not double-free frames still in use.
pub fn register_grant_mapping(phys: u64, owner: AddressSpaceId) -> FrameId {
    // Lock order matches alloc_frame/dec_and_maybe_free/free_frame:
    // FRAME_REGISTRY before PHYS_TO_FRAME (consistency prevents deadlock).
    let mut reg = FRAME_REGISTRY.lock();
    let mut phys_map = PHYS_TO_FRAME.lock();

    if let Some(&existing) = phys_map.get(&phys) {
        if let Some(entry) = reg.get_mut(&existing) {
            entry.map_count = entry.map_count.saturating_add(1);
        }
        return existing;
    }

    let id = FrameId::new(NEXT_FRAME_ID.fetch_add(1, Ordering::SeqCst));
    reg.insert(
        id,
        FrameEntry {
            phys_addr: phys,
            owner_space: owner,
            // Source already had the frame mapped; new target adds one more.
            map_count: 2,
            auto_free: true,
            page_count: 1,
        },
    );
    phys_map.insert(phys, id);
    id
}

/// Decrement mapping count; if the entry was auto-free (grant-tracked)
/// and the count reaches zero, free the frame back to PMM and remove it
/// from the registry.
///
/// For user-held frame tokens (`auto_free = false`), this is equivalent
/// to `dec_map_count` — the backing frame remains allocated until an
/// explicit `FrameFree` call.
pub fn dec_and_maybe_free(frame_id: FrameId) {
    let mut reg = FRAME_REGISTRY.lock();
    let Some(entry) = reg.get_mut(&frame_id) else {
        return;
    };
    entry.map_count = entry.map_count.saturating_sub(1);
    if !entry.auto_free || entry.map_count != 0 {
        return;
    }

    let phys = entry.phys_addr;
    let page_count = entry.page_count;
    reg.remove(&frame_id);
    drop(reg);

    PHYS_TO_FRAME.lock().remove(&phys);
    let order = ceil_log2_pages(page_count as usize);
    // Phase 1: advisory retype before PMM free.
    let _ = crate::mm::frame_table::retype_to_untyped(phys);
    crate::mm::pmm::free_order_tagged(phys, order as usize, "registry_dec");
}

/// Free a tracked frame. Fails if `map_count > 0`.
///
/// Removes from registry and calls `pmm::free_frame()`.
pub fn free_frame(frame_id: FrameId) -> Result<(), FrameRegistryError> {
    let mut reg = FRAME_REGISTRY.lock();
    let entry = reg.get(&frame_id).ok_or(FrameRegistryError::NotFound)?;

    if entry.map_count > 0 {
        return Err(FrameRegistryError::StillMapped);
    }

    let phys = entry.phys_addr;
    let page_count = entry.page_count;
    reg.remove(&frame_id);
    drop(reg);

    PHYS_TO_FRAME.lock().remove(&phys);
    let order = ceil_log2_pages(page_count as usize);
    // Phase 1: advisory retype before PMM free.
    let _ = crate::mm::frame_table::retype_to_untyped(phys);
    crate::mm::pmm::free_order(phys, order as usize);
    Ok(())
}

/// Get the physical address for a frame.
pub fn get_phys(frame_id: FrameId) -> Option<u64> {
    FRAME_REGISTRY.lock().get(&frame_id).map(|e| e.phys_addr)
}

/// Increment mapping count (called when frame is mapped into a space).
pub fn inc_map_count(frame_id: FrameId) {
    if let Some(entry) = FRAME_REGISTRY.lock().get_mut(&frame_id) {
        entry.map_count += 1;
    }
}

/// Decrement mapping count (called when frame is unmapped from a space).
pub fn dec_map_count(frame_id: FrameId) {
    if let Some(entry) = FRAME_REGISTRY.lock().get_mut(&frame_id) {
        entry.map_count = entry.map_count.saturating_sub(1);
    }
}

/// Reverse lookup: physical address → FrameId.
///
/// Used by `invoke_space_unmap` to determine if a frame is tracked.
pub fn lookup_by_phys(phys: u64) -> Option<FrameId> {
    PHYS_TO_FRAME.lock().get(&phys).copied()
}

/// Number of tracked frames in the registry.
pub fn tracked_count() -> usize {
    FRAME_REGISTRY.lock().len()
}

/// Sum of map counts across all tracked frames.
pub fn total_map_count() -> u64 {
    FRAME_REGISTRY
        .lock()
        .values()
        .map(|entry| entry.map_count as u64)
        .sum()
}

/// Allocate `n_pages` contiguous physical pages and register them as one frame.
///
/// Uses the buddy allocator's `alloc_order` to obtain a power-of-two block
/// large enough for `n_pages`. The allocated `page_count` is the rounded-up
/// power of two (e.g. 5 pages → order 3 → 8 pages allocated).
///
/// Returns `(FrameId, phys_base)`.
pub fn alloc_frame_n(owner: AddressSpaceId, n_pages: usize) -> Option<(FrameId, u64)> {
    if n_pages == 0 {
        return None;
    }
    let order = ceil_log2_pages(n_pages);
    if order > 12 {
        return None; // buddy max = order 12 (16 MiB), PMM MAX_ORDER
    }
    let phys = crate::mm::pmm::alloc_order(order as usize)?;
    // Phase 2.5: retype EVERY constituent page as UserData. LOUD on error.
    let allocated_count = 1usize << order;
    for i in 0..allocated_count {
        let page_phys = phys + (i as u64) * 4096;
        if let Err(e) = crate::mm::frame_table::retype_to_user(page_phys, owner) {
            klibcluu::error("frame_registry::alloc_frame_n: retype_to_user failed");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=0x", page_phys);
            klibcluu::log_dec(klibcluu::LogLevel::Error, "  owner=", owner.as_u64());
            let _ = e;
        }
    }
    let id = FrameId::new(NEXT_FRAME_ID.fetch_add(1, Ordering::SeqCst));
    let allocated_pages: u32 = 1u32 << order;
    let entry = FrameEntry {
        phys_addr: phys,
        owner_space: owner,
        map_count: 0,
        auto_free: false,
        page_count: allocated_pages,
    };
    FRAME_REGISTRY.lock().insert(id, entry);
    PHYS_TO_FRAME.lock().insert(phys, id);
    Some((id, phys))
}

/// `ceil(log2(n))` for `n >= 1`. Returns 0 for n = 1.
///
/// n=1 → 0, n=2 → 1, n=3..4 → 2, n=5..8 → 3, ...
fn ceil_log2_pages(n: usize) -> u32 {
    if n <= 1 {
        return 0;
    }
    (usize::BITS - (n - 1).leading_zeros()) as u32
}
