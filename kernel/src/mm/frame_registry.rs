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
}

/// Forward map: FrameId → FrameEntry
static FRAME_REGISTRY: Mutex<BTreeMap<FrameId, FrameEntry>> = Mutex::new(BTreeMap::new());

/// Reverse map: physical address → FrameId (for SpaceUnmap lookups)
static PHYS_TO_FRAME: Mutex<BTreeMap<u64, FrameId>> = Mutex::new(BTreeMap::new());

/// Monotonic frame ID counter
static NEXT_FRAME_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate a new physical frame and register it.
///
/// Calls `pmm::alloc_frame()` internally. Returns `(FrameId, phys_addr)`.
pub fn alloc_frame(owner: AddressSpaceId) -> Option<(FrameId, u64)> {
    let phys = crate::mm::pmm::alloc_frame()?;
    let id = FrameId::new(NEXT_FRAME_ID.fetch_add(1, Ordering::SeqCst));

    let entry = FrameEntry {
        phys_addr: phys,
        owner_space: owner,
        map_count: 0,
    };

    FRAME_REGISTRY.lock().insert(id, entry);
    PHYS_TO_FRAME.lock().insert(phys, id);

    Some((id, phys))
}

/// Free a tracked frame. Fails if `map_count > 0`.
///
/// Removes from registry and calls `pmm::free_frame()`.
pub fn free_frame(frame_id: FrameId) -> Result<(), ()> {
    let mut reg = FRAME_REGISTRY.lock();
    let entry = reg.get(&frame_id).ok_or(())?;

    if entry.map_count > 0 {
        return Err(());
    }

    let phys = entry.phys_addr;
    reg.remove(&frame_id);
    drop(reg);

    PHYS_TO_FRAME.lock().remove(&phys);
    crate::mm::pmm::free_frame(phys);
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
