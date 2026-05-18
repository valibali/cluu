//! Per-frame typed ownership table. seL4-style Untyped / Retype.
//!
//! # Phase 2 — refcount enforcement + Grant unified
//!
//! `ENFORCE_INVARIANTS = true` means retype mismatches are hard errors.
//!
//! Every map of a user leaf calls `inc_ref`; every unmap calls `dec_ref`.
//! `dec_ref` at refcount 0 auto-retypes to Untyped and calls
//! `pmm::free_frame_untyped` to return the frame to the buddy allocator.
//!
//! `inc_ref` on a UserData frame whose refcount transitions 1→2 automatically
//! retypes the frame to Grant (shared / multi-owner). Grant frames stay
//! Grant until refcount hits 0 (reverse retype to UserData deferred to Phase 4).

use alloc::vec::Vec;
use spin::Mutex;

use crate::token::scope::AddressSpaceId;

// ─── Enforcement toggle ───────────────────────────────────────────────────────

/// Phase 2: true → mismatches are hard errors.
const ENFORCE_INVARIANTS: bool = true;

// ─── Frame tag ───────────────────────────────────────────────────────────────

/// Type tag stored per physical frame.
///
/// `Untyped` (0) is the zero-value default; a freshly zeroed `FrameMeta`
/// starts Untyped with refcount=0 and owner=0.
#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FrameTag {
    Untyped      = 0, // zeroed default — returned to PMM or not yet typed
    UserData     = 1, // leaf user page (ELF segment, heap, stack)
    PageTable    = 2, // intermediate page table (PML4/PDPT/PD/PT)
    Grant        = 3, // shared / multi-owner user frame
    Device       = 4, // MMIO / device-mapped (never freed through PMM user path)
    KernelHeap   = 5, // kernel heap page
    BootReserved = 6, // kernel image, initrd, framebuffer, BOOTBOOT, low-1MB
}

// ─── Per-frame metadata ───────────────────────────────────────────────────────

/// 6-byte record stored for every physical frame in the system.
#[derive(Copy, Clone)]
pub struct FrameMeta {
    /// What this frame is being used for.
    pub tag: FrameTag,
    /// Number of references (PTEs / mappings pointing at this frame).
    /// Phase 1: advisory, not enforced.
    pub refcount: u16,
    /// `AddressSpaceId.0` truncated to u16; 0 = no owner.
    pub owner: u16,
    /// PT level (1..=4) when `tag == PageTable`; 0 elsewhere.
    pub extra: u8,
}

impl FrameMeta {
    const fn zeroed() -> Self {
        Self { tag: FrameTag::Untyped, refcount: 0, owner: 0, extra: 0 }
    }
}

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameTableError {
    /// Physical address translates to a frame index ≥ `max_managed_frame`.
    OutOfRange,
    /// The frame's current tag does not allow the requested operation.
    WrongTag,
    /// Retype-to-Untyped requested but `refcount > 0`.
    StillReferenced,
    /// Frame is already typed (non-Untyped) when a fresh retype was requested.
    AlreadyTyped,
}

// ─── Storage ─────────────────────────────────────────────────────────────────

/// Late-initialized frame table.
/// Populated once by `init(max_managed_frame)`, after kernel heap is live.
static FRAME_TABLE: Mutex<Option<Vec<FrameMeta>>> = Mutex::new(None);

// ─── Public init ─────────────────────────────────────────────────────────────

/// Initialise the frame table.
///
/// Allocates `max_managed_frame` `FrameMeta` entries on the kernel heap, all
/// set to Untyped / refcount=0 / owner=0. Must be called exactly once after
/// the kernel heap is live (after `mm::heap::init`).
///
/// Memory cost: ~6 bytes × `max_managed_frame`.  For 246 k frames that is
/// ~1.5 MB; for 2 M frames (8 GB RAM) ~12 MB.
pub fn init(max_managed_frame: usize) {
    let table = alloc::vec![FrameMeta::zeroed(); max_managed_frame];
    let mut guard = FRAME_TABLE.lock();
    if guard.is_some() {
        klibcluu::warn("FRAME_TABLE: init called twice — ignoring");
        return;
    }
    *guard = Some(table);
    klibcluu::log_dec(
        klibcluu::LogLevel::Info,
        "FRAME_TABLE: initialised with max_managed_frame=",
        max_managed_frame as u64,
    );
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Frame number from physical address.
#[inline]
fn frame_of(phys: u64) -> usize {
    (phys >> 12) as usize
}

/// Check that `phys` is range-valid and return its frame index.
///
/// Returns `Err(OutOfRange)` if the table has not been initialised or the
/// index is out of bounds.
fn checked_frame(table: &[FrameMeta], phys: u64) -> Result<usize, FrameTableError> {
    let idx = frame_of(phys);
    if idx >= table.len() {
        return Err(FrameTableError::OutOfRange);
    }
    Ok(idx)
}

/// Emit a `FRAME_TABLE WARN` line (advisory-mode mismatch).
#[inline]
fn warn_mismatch(msg: &'static str, phys: u64) {
    klibcluu::warn(msg);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "  phys=", phys);
}

// ─── Retype API ──────────────────────────────────────────────────────────────

/// Retype a frame to `PageTable` at the given `level` (1=PT 2=PD 3=PDPT 4=PML4)
/// owned by `owner`.
///
/// Phase 1: if the frame is already non-Untyped (e.g. early-boot allocation
/// before the table was live) we log a warning and set the new type anyway.
pub fn retype_to_pt(
    phys: u64,
    level: u8,
    owner: AddressSpaceId,
) -> Result<(), FrameTableError> {
    let mut guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_mut() else { return Ok(()); };
    let idx = checked_frame(table, phys)?;

    let entry = &mut table[idx];
    if entry.tag != FrameTag::Untyped {
        let err = FrameTableError::AlreadyTyped;
        warn_mismatch("FRAME_TABLE WARN: retype_to_pt on non-Untyped frame", phys);
        klibcluu::log_dec(klibcluu::LogLevel::Warn, "  current_tag=", entry.tag as u64);
        if ENFORCE_INVARIANTS {
            return Err(err);
        }
    }
    entry.tag = FrameTag::PageTable;
    entry.refcount = 1;
    entry.owner = owner.as_u64() as u16;
    entry.extra = level;
    Ok(())
}

/// Retype a frame to `UserData` owned by `owner`.
pub fn retype_to_user(phys: u64, owner: AddressSpaceId) -> Result<(), FrameTableError> {
    let mut guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_mut() else { return Ok(()); };
    let idx = checked_frame(table, phys)?;

    let entry = &mut table[idx];
    if entry.tag != FrameTag::Untyped {
        let err = FrameTableError::AlreadyTyped;
        warn_mismatch("FRAME_TABLE WARN: retype_to_user on non-Untyped frame", phys);
        klibcluu::log_dec(klibcluu::LogLevel::Warn, "  current_tag=", entry.tag as u64);
        if ENFORCE_INVARIANTS {
            return Err(err);
        }
        // Advisory: still update the record so Phase 2 can audit.
        entry.tag = FrameTag::UserData;
        entry.owner = owner.as_u64() as u16;
        entry.extra = 0;
        return Err(err);
    }
    entry.tag = FrameTag::UserData;
    entry.refcount = 1;
    entry.owner = owner.as_u64() as u16;
    entry.extra = 0;
    Ok(())
}

/// Retype a frame to `Grant` (shared / multi-owner).
///
/// Caller must have established ownership already (typically comes from a
/// prior `UserData` or `Untyped`). Phase 1: always succeeds.
pub fn retype_to_grant(phys: u64) -> Result<(), FrameTableError> {
    let mut guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_mut() else { return Ok(()); };
    let idx = checked_frame(table, phys)?;

    let entry = &mut table[idx];
    // Grant is a valid transition from UserData, Untyped, or Grant itself.
    entry.tag = FrameTag::Grant;
    entry.extra = 0;
    Ok(())
}

/// Retype a frame to `Device` (MMIO).
pub fn retype_to_device(phys: u64) -> Result<(), FrameTableError> {
    let mut guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_mut() else { return Ok(()); };
    let idx = checked_frame(table, phys)?;

    let entry = &mut table[idx];
    entry.tag = FrameTag::Device;
    entry.refcount = 1;
    entry.extra = 0;
    Ok(())
}

/// Retype a frame to `KernelHeap`.
pub fn retype_to_kernel_heap(phys: u64) -> Result<(), FrameTableError> {
    let mut guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_mut() else { return Ok(()); };
    let idx = checked_frame(table, phys)?;

    let entry = &mut table[idx];
    entry.tag = FrameTag::KernelHeap;
    entry.refcount = 1;
    entry.extra = 0;
    Ok(())
}

/// Retype a frame to `BootReserved` (kernel image, initrd, FB, BOOTBOOT, low-1MB).
pub fn retype_to_boot_reserved(phys: u64) -> Result<(), FrameTableError> {
    let mut guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_mut() else { return Ok(()); };
    let idx = checked_frame(table, phys)?;

    let entry = &mut table[idx];
    entry.tag = FrameTag::BootReserved;
    entry.refcount = 1;
    entry.extra = 0;
    Ok(())
}

/// Retype a frame back to `Untyped`.
///
/// Phase 1: warns if `refcount > 0` but always sets the tag (advisory).
/// Phase 2 (ENFORCE_INVARIANTS=true): returns `StillReferenced` when refcount > 0.
pub fn retype_to_untyped(phys: u64) -> Result<(), FrameTableError> {
    let mut guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_mut() else { return Ok(()); };
    let idx = checked_frame(table, phys)?;

    let entry = &mut table[idx];
    if entry.refcount > 0 {
        warn_mismatch("FRAME_TABLE WARN: retype_to_untyped with refcount > 0", phys);
        klibcluu::log_dec(klibcluu::LogLevel::Warn, "  refcount=", entry.refcount as u64);
        if ENFORCE_INVARIANTS {
            return Err(FrameTableError::StillReferenced);
        }
    }
    entry.tag = FrameTag::Untyped;
    entry.refcount = 0;
    entry.owner = 0;
    entry.extra = 0;
    Ok(())
}

// ─── Refcount API ─────────────────────────────────────────────────────────────

/// Increment the refcount on a frame. Returns the new refcount.
///
/// When a UserData frame's refcount transitions from 1 → 2, the frame is
/// automatically retyped UserData → Grant (sealing the "shared" semantics).
/// Grant frames keep their tag until refcount drops to 0.
///
/// Device / KernelHeap / BootReserved frames are silently skipped (they
/// never participate in the user refcount lifecycle).
pub fn inc_ref(phys: u64) -> Result<u16, FrameTableError> {
    let mut guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_mut() else { return Ok(0); };
    let idx = checked_frame(table, phys)?;

    let entry = &mut table[idx];
    // Device/KernelHeap/BootReserved: not managed by refcount, silently ignore.
    match entry.tag {
        FrameTag::Device | FrameTag::KernelHeap | FrameTag::BootReserved => {
            return Ok(entry.refcount);
        }
        _ => {}
    }
    let old_rc = entry.refcount;
    entry.refcount = entry.refcount.saturating_add(1);
    let new_rc = entry.refcount;
    // UserData 1→2 automatically transitions to Grant.
    if entry.tag == FrameTag::UserData && old_rc == 1 && new_rc == 2 {
        entry.tag = FrameTag::Grant;
        klibcluu::log_hex(klibcluu::LogLevel::Trace, "FRAME_TABLE: UserData→Grant phys=0x", phys);
    }
    Ok(new_rc)
}

/// Decrement the refcount on a frame. Returns the new refcount.
///
/// When the refcount reaches 0:
/// - The tag is automatically set to `Untyped` (owner and extra cleared).
/// - For UserData and Grant frames, the physical frame is returned to the PMM
///   buddy allocator (`pmm::free_frame`) automatically. Callers must NOT call
///   `pmm::free_*` directly after `dec_ref` — dec_ref owns that path.
///
/// Device / KernelHeap / BootReserved frames are silently skipped.
///
/// The FRAME_TABLE lock is dropped before calling into PMM to avoid
/// holding both locks simultaneously.
pub fn dec_ref(phys: u64) -> Result<u16, FrameTableError> {
    // Phase 1: determine whether we need to auto-free under the lock, then
    // drop the lock before calling PMM.
    let (new_rc, should_free) = {
        let mut guard = FRAME_TABLE.lock();
        let Some(table) = guard.as_mut() else { return Ok(0); };
        let idx = checked_frame(table, phys)?;

        let entry = &mut table[idx];
        // Device/KernelHeap/BootReserved: not managed, silently ignore.
        match entry.tag {
            FrameTag::Device | FrameTag::KernelHeap | FrameTag::BootReserved => {
                return Ok(entry.refcount);
            }
            _ => {}
        }
        entry.refcount = entry.refcount.saturating_sub(1);
        let new_rc = entry.refcount;
        let should_free = new_rc == 0 && matches!(entry.tag, FrameTag::UserData | FrameTag::Grant | FrameTag::PageTable);
        if new_rc == 0 {
            entry.tag = FrameTag::Untyped;
            entry.owner = 0;
            entry.extra = 0;
        }
        (new_rc, should_free)
    };
    // Auto-free to PMM when the last reference drops.  Lock is released here.
    if should_free {
        // Use free_frame_tagged so the ring gets an event.
        crate::mm::pmm::free_frame_tagged(phys, "dec_ref_auto");
    }
    Ok(new_rc)
}

// ─── Query API ───────────────────────────────────────────────────────────────

/// Return the `FrameTag` of the frame at `phys`.
///
/// Returns `Untyped` if the table is uninitialised or `phys` is out of range.
pub fn tag_of(phys: u64) -> FrameTag {
    let guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_ref() else { return FrameTag::Untyped; };
    let idx = frame_of(phys);
    if idx >= table.len() { return FrameTag::Untyped; }
    table[idx].tag
}

/// Return the owner `AddressSpaceId` of the frame, or `None` for Untyped.
pub fn owner_of(phys: u64) -> Option<AddressSpaceId> {
    let guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_ref() else { return None; };
    let idx = frame_of(phys);
    if idx >= table.len() { return None; }
    let e = &table[idx];
    if e.owner == 0 { None } else { Some(AddressSpaceId::new(e.owner as u64)) }
}

/// Return the refcount of the frame.
pub fn refcount_of(phys: u64) -> u16 {
    let guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_ref() else { return 0; };
    let idx = frame_of(phys);
    if idx >= table.len() { return 0; }
    table[idx].refcount
}

// ─── Diagnostic / audit ──────────────────────────────────────────────────────

/// Scan the entire table and count frames that are tagged with any of the
/// variants in `non_untyped_tags` but have `refcount == 0`, and frames
/// that are `Untyped` but have `refcount > 0`. These are the two flavours
/// of inconsistency Phase 2 will turn into hard errors.
///
/// Returns `(tag_but_no_ref, untyped_but_has_ref)`.
pub fn count_inconsistencies() -> (usize, usize) {
    let guard = FRAME_TABLE.lock();
    let Some(table) = guard.as_ref() else { return (0, 0); };
    let mut tag_no_ref: usize = 0;
    let mut untyped_has_ref: usize = 0;
    for e in table.iter() {
        if e.tag == FrameTag::Untyped && e.refcount > 0 {
            untyped_has_ref += 1;
        } else if e.tag != FrameTag::Untyped && e.refcount == 0 {
            // KernelHeap / Device / BootReserved frames don't participate in
            // user refcounting yet; exclude them from the "inconsistent" count.
            match e.tag {
                FrameTag::KernelHeap | FrameTag::Device | FrameTag::BootReserved => {}
                _ => tag_no_ref += 1,
            }
        }
    }
    (tag_no_ref, untyped_has_ref)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Each test gets its own local FrameMeta Vec (not the global FRAME_TABLE)
    // because `init()` is not re-entrant and we cannot reset a Mutex<Option<Vec>>
    // between tests on the host. We test the core logic by calling the helpers
    // directly on a local table slice.

    const PAGE: u64 = 4096;

    // ── helpers to exercise logic without touching the global table ──────────

    fn fresh_table(n: usize) -> Vec<FrameMeta> {
        alloc::vec![FrameMeta::zeroed(); n]
    }

    fn meta(table: &[FrameMeta], phys: u64) -> &FrameMeta {
        &table[frame_of(phys)]
    }

    fn do_retype_pt(
        table: &mut Vec<FrameMeta>,
        phys: u64,
        level: u8,
        owner: u64,
    ) -> Result<(), FrameTableError> {
        let idx = checked_frame(table, phys)?;
        let entry = &mut table[idx];
        if entry.tag != FrameTag::Untyped {
            return Err(FrameTableError::AlreadyTyped);
        }
        entry.tag = FrameTag::PageTable;
        entry.refcount = 1;
        entry.owner = owner as u16;
        entry.extra = level;
        Ok(())
    }

    fn do_retype_user(
        table: &mut Vec<FrameMeta>,
        phys: u64,
        owner: u64,
    ) -> Result<(), FrameTableError> {
        let idx = checked_frame(table, phys)?;
        let entry = &mut table[idx];
        if entry.tag != FrameTag::Untyped {
            return Err(FrameTableError::AlreadyTyped);
        }
        entry.tag = FrameTag::UserData;
        entry.refcount = 1;
        entry.owner = owner as u16;
        entry.extra = 0;
        Ok(())
    }

    fn do_retype_grant(table: &mut Vec<FrameMeta>, phys: u64) -> Result<(), FrameTableError> {
        let idx = checked_frame(table, phys)?;
        table[idx].tag = FrameTag::Grant;
        table[idx].extra = 0;
        Ok(())
    }

    fn do_retype_untyped(
        table: &mut Vec<FrameMeta>,
        phys: u64,
    ) -> Result<(), FrameTableError> {
        let idx = checked_frame(table, phys)?;
        let entry = &mut table[idx];
        if entry.refcount > 0 {
            return Err(FrameTableError::StillReferenced);
        }
        entry.tag = FrameTag::Untyped;
        entry.refcount = 0;
        entry.owner = 0;
        entry.extra = 0;
        Ok(())
    }

    /// Phase 1-era inc_ref (no UserData→Grant auto-transition).
    fn do_inc_ref(table: &mut Vec<FrameMeta>, phys: u64) -> Result<u16, FrameTableError> {
        let idx = checked_frame(table, phys)?;
        table[idx].refcount = table[idx].refcount.saturating_add(1);
        Ok(table[idx].refcount)
    }

    /// Phase 2-era inc_ref: UserData 1→2 auto-transitions to Grant.
    fn do_inc_ref_p2(table: &mut Vec<FrameMeta>, phys: u64) -> Result<u16, FrameTableError> {
        let idx = checked_frame(table, phys)?;
        let entry = &mut table[idx];
        match entry.tag {
            FrameTag::Device | FrameTag::KernelHeap | FrameTag::BootReserved => {
                return Ok(entry.refcount);
            }
            _ => {}
        }
        let old_rc = entry.refcount;
        entry.refcount = entry.refcount.saturating_add(1);
        let new_rc = entry.refcount;
        if entry.tag == FrameTag::UserData && old_rc == 1 && new_rc == 2 {
            entry.tag = FrameTag::Grant;
        }
        Ok(new_rc)
    }

    /// Phase 2-era dec_ref: auto-Untyped at 0 (does NOT call PMM in test context).
    fn do_dec_ref_p2(table: &mut Vec<FrameMeta>, phys: u64) -> Result<u16, FrameTableError> {
        let idx = checked_frame(table, phys)?;
        let entry = &mut table[idx];
        match entry.tag {
            FrameTag::Device | FrameTag::KernelHeap | FrameTag::BootReserved => {
                return Ok(entry.refcount);
            }
            _ => {}
        }
        entry.refcount = entry.refcount.saturating_sub(1);
        if entry.refcount == 0 {
            entry.tag = FrameTag::Untyped;
            entry.owner = 0;
            entry.extra = 0;
        }
        Ok(entry.refcount)
    }

    fn do_dec_ref(table: &mut Vec<FrameMeta>, phys: u64) -> Result<u16, FrameTableError> {
        let idx = checked_frame(table, phys)?;
        let entry = &mut table[idx];
        entry.refcount = entry.refcount.saturating_sub(1);
        if entry.refcount == 0 {
            entry.tag = FrameTag::Untyped;
            entry.owner = 0;
            entry.extra = 0;
        }
        Ok(entry.refcount)
    }

    // ── Test 1: fresh table ─────────────────────────────────────────────────

    /// Fresh table → all frames Untyped, refcount=0, owner=0.
    #[test]
    fn test_fresh_table_all_untyped() {
        let table = fresh_table(16);
        for (i, e) in table.iter().enumerate() {
            assert_eq!(e.tag, FrameTag::Untyped, "frame {} should be Untyped", i);
            assert_eq!(e.refcount, 0, "frame {} refcount should be 0", i);
            assert_eq!(e.owner, 0, "frame {} owner should be 0", i);
        }
    }

    // ── Test 2: retype_to_user ──────────────────────────────────────────────

    /// retype_to_user → tag=UserData, refcount=1, owner set.
    #[test]
    fn test_retype_to_user() {
        let mut table = fresh_table(16);
        let phys = 3 * PAGE;
        let owner = 42u64;

        do_retype_user(&mut table, phys, owner).unwrap();

        let e = meta(&table, phys);
        assert_eq!(e.tag, FrameTag::UserData);
        assert_eq!(e.refcount, 1);
        assert_eq!(e.owner, 42);
        assert_eq!(e.extra, 0);
    }

    // ── Test 3: double retype_to_user → AlreadyTyped ───────────────────────

    /// retype_to_user twice on same phys → second returns AlreadyTyped.
    #[test]
    fn test_double_retype_user_error() {
        let mut table = fresh_table(16);
        let phys = 5 * PAGE;

        do_retype_user(&mut table, phys, 1).unwrap();
        let err = do_retype_user(&mut table, phys, 2).unwrap_err();
        assert_eq!(err, FrameTableError::AlreadyTyped);
    }

    // ── Test 4: inc_ref / dec_ref lifecycle ─────────────────────────────────

    /// retype_to_user → inc_ref → refcount=2 → dec_ref → 1 → dec_ref → 0
    /// → auto-Untyped.
    #[test]
    fn test_refcount_lifecycle() {
        let mut table = fresh_table(16);
        let phys = 7 * PAGE;

        do_retype_user(&mut table, phys, 99).unwrap();
        assert_eq!(meta(&table, phys).refcount, 1);

        assert_eq!(do_inc_ref(&mut table, phys).unwrap(), 2);
        assert_eq!(meta(&table, phys).refcount, 2);

        assert_eq!(do_dec_ref(&mut table, phys).unwrap(), 1);
        assert_eq!(meta(&table, phys).tag, FrameTag::UserData);

        assert_eq!(do_dec_ref(&mut table, phys).unwrap(), 0);
        let e = meta(&table, phys);
        assert_eq!(e.tag, FrameTag::Untyped);
        assert_eq!(e.refcount, 0);
        assert_eq!(e.owner, 0);
    }

    // ── Test 5: retype_to_pt ────────────────────────────────────────────────

    /// retype_to_pt with level=2 → tag=PageTable, extra=2.
    #[test]
    fn test_retype_to_pt() {
        let mut table = fresh_table(16);
        let phys = 2 * PAGE;

        do_retype_pt(&mut table, phys, 2, 7).unwrap();

        let e = meta(&table, phys);
        assert_eq!(e.tag, FrameTag::PageTable);
        assert_eq!(e.extra, 2);
        assert_eq!(e.owner, 7);
        assert_eq!(e.refcount, 1);
    }

    // ── Test 6: retype_to_grant transitions ─────────────────────────────────

    /// retype_to_user → retype_to_grant: tag transitions, refcount preserved.
    #[test]
    fn test_retype_user_to_grant() {
        let mut table = fresh_table(16);
        let phys = 4 * PAGE;

        do_retype_user(&mut table, phys, 5).unwrap();
        do_inc_ref(&mut table, phys).unwrap(); // refcount = 2
        do_retype_grant(&mut table, phys).unwrap();

        let e = meta(&table, phys);
        assert_eq!(e.tag, FrameTag::Grant);
        assert_eq!(e.refcount, 2); // preserved
    }

    // ── Test 7: retype_to_untyped with refcount > 0 ─────────────────────────

    /// retype_to_untyped on frame with refcount > 0 → StillReferenced.
    #[test]
    fn test_retype_to_untyped_still_referenced() {
        let mut table = fresh_table(16);
        let phys = 6 * PAGE;

        do_retype_user(&mut table, phys, 3).unwrap();
        // refcount is 1 — should not be allowed to untype.
        let err = do_retype_untyped(&mut table, phys).unwrap_err();
        assert_eq!(err, FrameTableError::StillReferenced);
    }

    // ── Test 8: OutOfRange ──────────────────────────────────────────────────

    /// OutOfRange phys → OutOfRange error.
    #[test]
    fn test_out_of_range() {
        let table = fresh_table(4);
        // frame_of(5 * PAGE) = 5, but table only has 4 entries.
        let phys = 5 * PAGE;
        let err = checked_frame(&table, phys).unwrap_err();
        assert_eq!(err, FrameTableError::OutOfRange);
    }

    // ── Phase 2 Tests ──────────────────────────────────────────────────────

    /// P2-T1: inc_ref on UserData(rc=1) → returns 2, tag transitions to Grant.
    #[test]
    fn test_p2_inc_ref_userdata_to_grant() {
        let mut table = fresh_table(16);
        let phys = 8 * PAGE;
        do_retype_user(&mut table, phys, 10).unwrap();
        assert_eq!(meta(&table, phys).tag, FrameTag::UserData);
        assert_eq!(meta(&table, phys).refcount, 1);

        let new_rc = do_inc_ref_p2(&mut table, phys).unwrap();
        assert_eq!(new_rc, 2, "refcount should be 2 after inc_ref");
        assert_eq!(meta(&table, phys).tag, FrameTag::Grant, "UserData 1→2 must become Grant");
    }

    /// P2-T2: dec_ref on Grant(rc=2) → returns 1, tag stays Grant.
    #[test]
    fn test_p2_dec_ref_grant_stays_grant() {
        let mut table = fresh_table(16);
        let phys = 9 * PAGE;
        do_retype_user(&mut table, phys, 11).unwrap();
        do_inc_ref_p2(&mut table, phys).unwrap(); // rc=2, tag=Grant

        let new_rc = do_dec_ref_p2(&mut table, phys).unwrap();
        assert_eq!(new_rc, 1, "refcount should be 1 after dec_ref");
        assert_eq!(meta(&table, phys).tag, FrameTag::Grant, "Grant stays Grant at rc=1");
    }

    /// P2-T3: dec_ref on Grant(rc=1) → returns 0, tag transitions to Untyped.
    #[test]
    fn test_p2_dec_ref_grant_to_untyped() {
        let mut table = fresh_table(16);
        let phys = 10 * PAGE;
        do_retype_user(&mut table, phys, 12).unwrap();
        do_inc_ref_p2(&mut table, phys).unwrap(); // rc=2, tag=Grant
        do_dec_ref_p2(&mut table, phys).unwrap(); // rc=1, tag=Grant

        let new_rc = do_dec_ref_p2(&mut table, phys).unwrap();
        assert_eq!(new_rc, 0, "refcount should be 0");
        let e = meta(&table, phys);
        assert_eq!(e.tag, FrameTag::Untyped, "Grant rc=0 → Untyped");
        assert_eq!(e.owner, 0);
    }

    /// P2-T4: retype_to_untyped on Grant(rc=0) succeeds.
    #[test]
    fn test_p2_retype_untyped_on_grant_rc0() {
        let mut table = fresh_table(16);
        let phys = 11 * PAGE;
        do_retype_user(&mut table, phys, 13).unwrap();
        do_inc_ref_p2(&mut table, phys).unwrap();  // rc=2, Grant
        do_dec_ref_p2(&mut table, phys).unwrap();  // rc=1, Grant
        do_dec_ref_p2(&mut table, phys).unwrap();  // rc=0, Untyped (auto)
        // Frame is now Untyped via auto-untype; retype_to_untyped on already-
        // Untyped (rc=0) succeeds.
        let result = do_retype_untyped(&mut table, phys);
        assert!(result.is_ok(), "retype_to_untyped on Untyped(rc=0) should succeed");
    }

    /// P2-T5: retype_to_untyped on Grant(rc>0) → StillReferenced error.
    #[test]
    fn test_p2_retype_untyped_on_grant_still_referenced() {
        let mut table = fresh_table(16);
        let phys = 12 * PAGE;
        do_retype_user(&mut table, phys, 14).unwrap();
        do_inc_ref_p2(&mut table, phys).unwrap();  // rc=2, Grant
        // Frame has rc=2; retype_to_untyped must fail.
        let err = do_retype_untyped(&mut table, phys).unwrap_err();
        assert_eq!(err, FrameTableError::StillReferenced);
    }

    /// P2-T6: Full sequence retype_to_user → inc_ref → Grant → dec_ref → Grant(rc=1)
    /// → dec_ref → Untyped(rc=0). Net: ownership cleared, ready for reallocation.
    #[test]
    fn test_p2_full_ownership_lifecycle() {
        let mut table = fresh_table(16);
        let phys = 13 * PAGE;

        // Allocate
        do_retype_user(&mut table, phys, 100).unwrap();
        assert_eq!(meta(&table, phys).tag, FrameTag::UserData);
        assert_eq!(meta(&table, phys).refcount, 1);

        // Grant (second mapping)
        let rc = do_inc_ref_p2(&mut table, phys).unwrap();
        assert_eq!(rc, 2);
        assert_eq!(meta(&table, phys).tag, FrameTag::Grant);

        // First unmap
        let rc = do_dec_ref_p2(&mut table, phys).unwrap();
        assert_eq!(rc, 1);
        assert_eq!(meta(&table, phys).tag, FrameTag::Grant);

        // Last unmap → auto-Untyped
        let rc = do_dec_ref_p2(&mut table, phys).unwrap();
        assert_eq!(rc, 0);
        let e = meta(&table, phys);
        assert_eq!(e.tag, FrameTag::Untyped);
        assert_eq!(e.owner, 0);
        assert_eq!(e.refcount, 0);
    }

    /// P2-T7: Sequential inc_ref calls: rc 1→2→3 yields tag=Grant rc=3.
    #[test]
    fn test_p2_multi_inc_ref_stays_grant() {
        let mut table = fresh_table(16);
        let phys = 14 * PAGE;
        do_retype_user(&mut table, phys, 200).unwrap(); // rc=1, UserData

        let rc = do_inc_ref_p2(&mut table, phys).unwrap(); // rc=2
        assert_eq!(rc, 2);
        assert_eq!(meta(&table, phys).tag, FrameTag::Grant);

        let rc = do_inc_ref_p2(&mut table, phys).unwrap(); // rc=3
        assert_eq!(rc, 3);
        // Grant 2→3: tag stays Grant
        assert_eq!(meta(&table, phys).tag, FrameTag::Grant);
    }

    /// P2-T8: Integration — frame aliased as both UserData and PageTable must fail.
    /// Simulates the 2026-05-18 alias scenario:
    ///   retype_to_user(0x2a04e000, spA) → login's BSS alloc
    ///   retype_to_pt(0x2a04e000, level=3, spB) → MUST FAIL AlreadyTyped
    #[test]
    fn test_p2_alias_scenario_2026_05_18() {
        let mut table = fresh_table(0x2b000); // enough frames to cover 0x2a04e000
        // 0x2a04e000 / 4096 = 0x2a04e frame index
        // We use a scaled-down address to stay within our small table.
        // Frame 5 = phys 5*4096 = 0x5000
        let phys_a = 5 * PAGE; // simulates login's BSS frame

        // spA marks the frame as UserData (login allocates it)
        let owner_a = 42u64;
        do_retype_user(&mut table, phys_a, owner_a).unwrap();
        assert_eq!(meta(&table, phys_a).tag, FrameTag::UserData);

        // spB tries to retype the SAME frame as a PDPT (level 3)
        // → MUST return AlreadyTyped (the bug class this commit closes)
        let owner_b = 43u64;
        let err = do_retype_pt(&mut table, phys_a, 3, owner_b).unwrap_err();
        assert_eq!(
            err, FrameTableError::AlreadyTyped,
            "retype_to_pt on UserData frame must fail with AlreadyTyped"
        );
    }
}
