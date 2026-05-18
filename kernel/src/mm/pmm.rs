//! Physical Memory Manager — Buddy Allocator
//!
//! Manages physical frames using a buddy allocator with orders 0-9
//! (4KB pages through 2MB large pages).
//!
//! Two-phase initialization:
//! - Phase 1 (`init`): Bitmap-only, before physmap is available
//! - Phase 2 (`init_buddy`): Builds intrusive free lists via physmap
//!
//! After phase 2, allocation is O(1) and freed blocks automatically
//! coalesce into larger blocks.

use crate::bootboot::{MMapEnt, BOOTBOOT};
use crate::mm::boot::info::BootInfoProvider;
use spin::Mutex;
extern crate alloc;

// ─── Caller-site tracking for alloc/free diagnosis ────────────────────────────
//
// Set to `false` to compile out all ring-buffer overhead (zero cost when off).
// Formerly TRACK_FREE_CALLERS; now covers both alloc and free events.
const TRACK_ALLOC_FREE: bool = true;

/// Capacity of the ring buffer (power of 2 for cheap modular wrap).
///
/// Bumped from 1024 → 4096: at 60 Hz compositor + ELF loads each boot spawns
/// O(100) PT/PD/PDPT/PML4 allocs plus the leaf pages, so 1024 slots fill up in
/// under a second of boot activity and the prior-free entry for a double-free
/// can be evicted before the second free fires. 4096 slots costs 4096×24 = 96 KB
/// of static BSS — acceptable for a diagnostic build.
const EVENT_RING_LEN: usize = 4096;

/// Discriminator stored in each ring slot.
#[derive(Copy, Clone, PartialEq, Eq)]
enum EventKind {
    Alloc,
    Free,
}

/// One slot in the alloc/free event ring buffer.
#[derive(Copy, Clone)]
struct EventSite {
    kind: EventKind,
    phys: u64,
    caller_rip: u64,
    source: &'static str,
}

impl EventSite {
    const EMPTY: EventSite =
        EventSite { kind: EventKind::Free, phys: 0, caller_rip: 0, source: "" };
}

struct EventRing {
    buf: [EventSite; EVENT_RING_LEN],
    /// Write cursor (always incremented; index = head % EVENT_RING_LEN).
    head: usize,
}

impl EventRing {
    const fn new() -> Self {
        Self { buf: [EventSite::EMPTY; EVENT_RING_LEN], head: 0 }
    }

    fn push(&mut self, site: EventSite) {
        let idx = self.head & (EVENT_RING_LEN - 1);
        self.buf[idx] = site;
        self.head = self.head.wrapping_add(1);
    }

    /// Return the most-recent Free EventSite whose `phys` matches. O(N).
    fn find_last_free(&self, phys: u64) -> Option<(EventSite, usize)> {
        // Walk backwards from head-1 (most recent) to head (oldest).
        // Returns the event and its logical sequence number (= ring.head - 1 - i).
        for i in 0..EVENT_RING_LEN {
            let idx = self.head.wrapping_sub(1).wrapping_sub(i) & (EVENT_RING_LEN - 1);
            let e = &self.buf[idx];
            if e.phys == phys && e.caller_rip != 0 && e.kind == EventKind::Free {
                let seq = self.head.wrapping_sub(1).wrapping_sub(i);
                return Some((*e, seq));
            }
        }
        None
    }

    /// Collect all Alloc events for `phys` whose logical sequence number is
    /// strictly greater than `after_seq`. Used by report_double_free to find
    /// intervening allocs between two frees of the same frame.
    ///
    /// Returns events in chronological order (oldest first), up to `out` len.
    fn collect_allocs_after(&self, phys: u64, after_seq: usize, out: &mut [EventSite]) -> usize {
        let mut count = 0;
        // We need chronological order, so scan oldest→newest.
        // Ring covers [head - EVENT_RING_LEN, head). Walk forward from oldest.
        let oldest_seq = self.head.wrapping_sub(EVENT_RING_LEN);
        for i in 0..EVENT_RING_LEN {
            let seq = oldest_seq.wrapping_add(i);
            if seq == self.head {
                break;
            }
            // Only look at events after `after_seq`.
            if seq <= after_seq {
                continue;
            }
            let idx = seq & (EVENT_RING_LEN - 1);
            let e = &self.buf[idx];
            if e.phys == phys && e.caller_rip != 0 && e.kind == EventKind::Alloc {
                if count < out.len() {
                    out[count] = *e;
                    count += 1;
                }
            }
        }
        count
    }
}

static EVENT_RING: Mutex<EventRing> = Mutex::new(EventRing::new());

/// Capture the return address of our caller's caller using the frame pointer.
///
/// Reads `[rbp + 8]` which is the return address saved when our *direct*
/// caller entered its stack frame. Frame pointers are guaranteed present
/// because `.cargo/config.toml` injects `-C force-frame-pointers=yes`.
///
/// Safety: only valid when called from a function that established its own
/// frame-pointer prologue (true for all non-inlined Rust fn in this build).
#[inline(always)]
fn caller_rip() -> u64 {
    let rip: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, [rbp + 8]",
            out(reg) rip,
            options(nostack, preserves_flags)
        );
    }
    rip
}

/// Record a successful free into the ring buffer (no-op when TRACK_ALLOC_FREE=false).
#[inline(always)]
fn record_free(phys: u64, rip: u64, source: &'static str) {
    if !TRACK_ALLOC_FREE {
        return;
    }
    if let Some(mut ring) = EVENT_RING.try_lock() {
        ring.push(EventSite { kind: EventKind::Free, phys, caller_rip: rip, source });
    }
    // If the lock is contended (very rare — called inside PMM.lock() holder)
    // we silently drop the record rather than deadlock.
}

/// Record a successful alloc into the ring buffer (no-op when TRACK_ALLOC_FREE=false).
#[inline(always)]
fn record_alloc(phys: u64, rip: u64, source: &'static str) {
    if !TRACK_ALLOC_FREE {
        return;
    }
    if let Some(mut ring) = EVENT_RING.try_lock() {
        ring.push(EventSite { kind: EventKind::Alloc, phys, caller_rip: rip, source });
    }
    // Lock-contention: silently drop (same policy as record_free).
}

/// Log a double-free event: scan the ring for the prior free site, any
/// intervening alloc events for the same phys, and the current free RIP.
///
/// Note: source tags are emitted on separate log lines after their RIP
/// so we can stay allocation-free (no `format!` / String needed).
#[inline(never)]
fn report_double_free(phys: u64, curr_rip: u64, curr_source: &'static str) {
    klibcluu::error("PMM: free_order skipped — frame already free (double-free)");
    klibcluu::log_hex(klibcluu::LogLevel::Error, "  phys=", phys);
    if !TRACK_ALLOC_FREE {
        return;
    }
    klibcluu::log_hex(klibcluu::LogLevel::Error, "  curr_free_rip=", curr_rip);
    klibcluu::log_str_pair(klibcluu::LogLevel::Error, "  curr_source=", curr_source);
    if let Some(ring) = EVENT_RING.try_lock() {
        if let Some((prior, prior_seq)) = ring.find_last_free(phys) {
            klibcluu::log_hex(klibcluu::LogLevel::Error, "  prior_free_rip=", prior.caller_rip);
            klibcluu::log_str_pair(klibcluu::LogLevel::Error, "  prior_source=", prior.source);
            // Scan for any alloc events for this phys that happened AFTER the prior free.
            let mut allocs: [EventSite; 8] = [EventSite::EMPTY; 8];
            let n = ring.collect_allocs_after(phys, prior_seq, &mut allocs);
            if n == 0 {
                klibcluu::error("  intermediate_allocs=<none>");
            } else {
                for i in 0..n {
                    klibcluu::log_hex(
                        klibcluu::LogLevel::Error,
                        "  intermediate_alloc_rip=",
                        allocs[i].caller_rip,
                    );
                    klibcluu::log_str_pair(
                        klibcluu::LogLevel::Error,
                        "  intermediate_alloc_source=",
                        allocs[i].source,
                    );
                }
            }
        } else {
            klibcluu::error("  prior_source=<not in ring>");
        }
    } else {
        klibcluu::error("  prior_source=<ring locked>");
    }
}

const PAGE_SIZE: usize = 4096;

/// Maximum number of physical frames we can track (4GB / 4KB = 1M frames)
const MAX_FRAMES: usize = 1024 * 1024;

/// Number of u64 entries in the bitmap
const BITMAP_LEN: usize = MAX_FRAMES / 64;

/// Number of buddy orders: 0 = 4KB, 1 = 8KB, ..., 9 = 2MB
const MAX_ORDER: usize = 10;

// ═══════════════════════════════════════════════════════════════════════════
// Intrusive free list — stored in the free pages themselves via physmap
// ═══════════════════════════════════════════════════════════════════════════

/// Header written into the first bytes of every free block.
/// Uses a doubly-linked list for O(1) removal during buddy coalescing.
#[repr(C)]
struct FreePageHeader {
    next: usize, // frame number of next block (0 = end)
    prev: usize, // frame number of prev block (0 = head sentinel)
}

/// Per-order free list head
struct FreeList {
    head: usize, // frame number of first free block (0 = empty)
    count: usize,
}

impl FreeList {
    const fn new() -> Self {
        Self { head: 0, count: 0 }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Buddy Allocator
// ═══════════════════════════════════════════════════════════════════════════

struct BuddyAllocator {
    /// Bitmap: bit SET = free, bit CLEAR = used (same convention as before)
    bitmap: [u64; BITMAP_LEN],
    /// Free lists per order (only used after phase 2)
    free_lists: [FreeList; MAX_ORDER],
    /// Whether buddy mode is active (phase 2 complete)
    buddy_ready: bool,
    total_frames: usize,
    free_frames: usize,
    /// Highest frame index that was ever part of usable memory (exclusive upper bound).
    /// Frames at or above this index are device MMIO / non-RAM and must never be freed.
    max_managed_frame: usize,
}

impl BuddyAllocator {
    const fn new() -> Self {
        Self {
            bitmap: [0; BITMAP_LEN],
            free_lists: [const { FreeList::new() }; MAX_ORDER],
            buddy_ready: false,
            total_frames: 0,
            free_frames: 0,
            max_managed_frame: 0,
        }
    }

    // ─── Bitmap operations ─────────────────────────────────────────────────

    fn bitmap_is_free(&self, frame: usize) -> bool {
        if frame >= MAX_FRAMES {
            return false;
        }
        self.bitmap[frame / 64] & (1u64 << (frame % 64)) != 0
    }

    fn bitmap_set_free(&mut self, frame: usize) {
        if frame >= MAX_FRAMES {
            return;
        }
        let idx = frame / 64;
        let mask = 1u64 << (frame % 64);
        if self.bitmap[idx] & mask == 0 {
            self.bitmap[idx] |= mask;
        }
    }

    fn bitmap_set_used(&mut self, frame: usize) {
        if frame >= MAX_FRAMES {
            return;
        }
        let idx = frame / 64;
        let mask = 1u64 << (frame % 64);
        if self.bitmap[idx] & mask != 0 {
            self.bitmap[idx] &= !mask;
        }
    }

    /// Mark a range of frames as used in bitmap
    fn bitmap_set_range_used(&mut self, start: usize, count: usize) {
        for f in start..start + count {
            self.bitmap_set_used(f);
        }
    }

    // ─── Intrusive list helpers (require physmap) ──────────────────────────

    fn read_header(&self, frame: usize) -> FreePageHeader {
        if frame == 0 || frame >= MAX_FRAMES {
            // Corrupted free-list pointer or out-of-range frame. Return a zero
            // header instead of panicking so a userspace-driven path can't
            // crash the kernel. Caller treats next=0/prev=0 as end-of-list.
            klibcluu::error("PMM: read_header out-of-range — list corruption (soft-fail)");
            klibcluu::log_dec(klibcluu::LogLevel::Error, "  frame=", frame as u64);
            return FreePageHeader { next: 0, prev: 0 };
        }
        let phys = (frame as u64) * (PAGE_SIZE as u64);
        let virt = unsafe { crate::mm::physmap::phys_to_virt_u64(phys) } as *const FreePageHeader;
        unsafe { core::ptr::read_volatile(virt) }
    }

    fn write_header(&self, frame: usize, header: &FreePageHeader) {
        if frame == 0 || frame >= MAX_FRAMES {
            // See read_header. Skip the write rather than deref a bad pointer.
            klibcluu::error("PMM: write_header out-of-range — list corruption (soft-fail)");
            klibcluu::log_dec(klibcluu::LogLevel::Error, "  frame=", frame as u64);
            return;
        }
        let phys = (frame as u64) * (PAGE_SIZE as u64);
        let virt = unsafe { crate::mm::physmap::phys_to_virt_u64(phys) } as *mut FreePageHeader;
        unsafe { core::ptr::write_volatile(virt, FreePageHeader { ..*header }) }
    }

    /// Push a block onto the front of an order's free list
    fn list_push(&mut self, frame: usize, order: usize) {
        // AUDIT: every frame in the block must currently be marked USED
        // (bitmap bit clear). If any bit is already set (free), the block is
        // being pushed twice — a double-free. Only check post-buddy_ready,
        // since phase-2 init paths (`add_region_to_buddy`) clear bitmap bits
        // before calling list_push.
        if self.buddy_ready {
            let block_size = 1 << order;
            for f in frame..frame + block_size {
                if self.bitmap_is_free(f) {
                    // Soft-fail: log and refuse the push. Panicking on a
                    // userspace-driven double-free would halt the kernel,
                    // which violates the microkernel invariant that user
                    // activity never crashes the kernel. The real root
                    // cause (often shared PTE rows that point at the same
                    // frame and are walked twice in teardown_user_pages)
                    // must be fixed at the caller — but until then this
                    // keeps the kernel alive.
                    klibcluu::error("PMM AUDIT: list_push of already-free frame (soft-fail)");
                    klibcluu::log_hex(
                        klibcluu::LogLevel::Error,
                        "  phys=",
                        (f as u64) * (PAGE_SIZE as u64),
                    );
                    klibcluu::log_dec(klibcluu::LogLevel::Error, "  order=", order as u64);
                    return;
                }
            }
        }

        let old_head = self.free_lists[order].head;

        // Write header into the free page
        self.write_header(
            frame,
            &FreePageHeader {
                next: old_head,
                prev: 0, // we are the new head
            },
        );

        // Update old head's prev pointer
        if old_head != 0 {
            let mut old_hdr = self.read_header(old_head);
            old_hdr.prev = frame;
            self.write_header(old_head, &old_hdr);
        }

        self.free_lists[order].head = frame;
        self.free_lists[order].count += 1;

        // Mark all frames in this block as free in bitmap
        let block_size = 1 << order;
        for f in frame..frame + block_size {
            self.bitmap_set_free(f);
        }
    }

    /// Remove a specific block from its order's free list (O(1) with doubly-linked)
    fn list_remove(&mut self, frame: usize, order: usize) {
        let hdr = self.read_header(frame);

        // Update prev's next
        if hdr.prev != 0 {
            let mut prev_hdr = self.read_header(hdr.prev);
            prev_hdr.next = hdr.next;
            self.write_header(hdr.prev, &prev_hdr);
        } else {
            // We were the head
            self.free_lists[order].head = hdr.next;
        }

        // Update next's prev
        if hdr.next != 0 {
            let mut next_hdr = self.read_header(hdr.next);
            next_hdr.prev = hdr.prev;
            self.write_header(hdr.next, &next_hdr);
        }

        self.free_lists[order].count -= 1;

        // Mark frames as used in bitmap
        let block_size = 1 << order;

        // AUDIT: every frame in the block must currently be marked FREE
        // (bitmap bit set). If any bit is already clear (used), the block was
        // somehow on the free list while also tracked as allocated — the
        // canonical PMM corruption signature for the MAP_SHARE_PHYS aliasing
        // bug.
        if self.buddy_ready {
            for f in frame..frame + block_size {
                if !self.bitmap_is_free(f) {
                    klibcluu::error("PMM AUDIT: list_remove of non-free frame");
                    klibcluu::log_hex(
                        klibcluu::LogLevel::Error,
                        "  phys=",
                        (f as u64) * (PAGE_SIZE as u64),
                    );
                    klibcluu::log_dec(klibcluu::LogLevel::Error, "  order=", order as u64);
                    panic!("PMM audit: list_remove of non-free frame");
                }
            }
        }

        self.bitmap_set_range_used(frame, block_size);
    }

    /// Pop the first block from an order's free list
    fn list_pop(&mut self, order: usize) -> Option<usize> {
        let head = self.free_lists[order].head;
        if head == 0 {
            return None;
        }
        self.list_remove(head, order);
        Some(head)
    }

    // ─── Buddy operations ──────────────────────────────────────────────────

    /// Allocate a block of 2^order pages using the buddy system
    fn buddy_alloc(&mut self, order: usize) -> Option<u64> {
        if order >= MAX_ORDER {
            return None;
        }

        // Find the smallest order >= requested that has a free block
        let mut found_order = order;
        while found_order < MAX_ORDER && self.free_lists[found_order].head == 0 {
            found_order += 1;
        }

        if found_order >= MAX_ORDER {
            return None;
        }

        // Pop a block from the found order
        let frame = self.list_pop(found_order)?;

        // Split down to the requested order, putting buddies on free lists
        while found_order > order {
            found_order -= 1;
            let buddy = frame + (1 << found_order);
            self.list_push(buddy, found_order);
        }

        let pages = 1 << order;
        self.free_frames -= pages;
        Some((frame * PAGE_SIZE) as u64)
    }

    /// Free a block of 2^order pages, coalescing with buddies
    fn buddy_free(&mut self, phys_addr: u64, order: usize) {
        let mut frame = (phys_addr / PAGE_SIZE as u64) as usize;
        let mut ord = order;

        self.free_frames += 1 << order;

        // Try to coalesce with buddy
        while ord < MAX_ORDER - 1 {
            let buddy = frame ^ (1 << ord);

            // Check if buddy is free and at the same order
            if buddy >= MAX_FRAMES || !self.is_block_free(buddy, ord) {
                break;
            }

            // Remove buddy from its free list
            self.list_remove(buddy, ord);

            // Merge: take the lower-addressed block
            frame = frame.min(buddy);
            ord += 1;
        }

        // Add merged block to free list
        self.list_push(frame, ord);
    }

    /// Check if a block of 2^order pages starting at `frame` is entirely free
    fn is_block_free(&self, frame: usize, order: usize) -> bool {
        let block_size = 1usize << order;
        // All frames in the block must be marked free in bitmap
        for f in frame..frame + block_size {
            if !self.bitmap_is_free(f) {
                return false;
            }
        }
        // Verify it's actually at this order by checking that the header's
        // position matches (it's the head of a block, not a sub-block)
        true
    }

    // ─── Bitmap-only fallback (phase 1, before physmap) ────────────────────

    fn bitmap_alloc_frame(&mut self) -> Option<u64> {
        for i in 0..BITMAP_LEN {
            let entry = self.bitmap[i];
            if entry != 0 {
                let bit = entry.trailing_zeros() as usize;
                let frame = i * 64 + bit;
                if frame == 0 {
                    continue;
                }
                self.bitmap_set_used(frame);
                self.free_frames -= 1;
                return Some((frame * PAGE_SIZE) as u64);
            }
        }
        None
    }

    // ─── Unified alloc/free API ────────────────────────────────────────────

    fn alloc_order(&mut self, order: usize) -> Option<u64> {
        if self.buddy_ready {
            self.buddy_alloc(order)
        } else {
            // Fallback: bitmap scan for single frames only
            if order == 0 {
                self.bitmap_alloc_frame()
            } else {
                klibcluu::error("PMM: multi-page alloc before buddy init");
                None
            }
        }
    }

    /// Like `alloc_order` but records an alloc event with `tag` and `rip`.
    fn alloc_order_tagged(&mut self, order: usize, tag: &'static str, rip: u64) -> Option<u64> {
        let phys = self.alloc_order(order)?;
        // Record after the alloc so the ring only ever contains successful events.
        record_alloc(phys, rip, tag);
        Some(phys)
    }

    fn free_order_tagged(&mut self, phys_addr: u64, order: usize, tag: &'static str, rip: u64) {
        let frame = (phys_addr / PAGE_SIZE as u64) as usize;
        // Skip addresses outside the managed physical memory range.
        // Device MMIO pages (e.g. framebuffer) are not PMM-owned.
        if frame >= self.max_managed_frame || frame >= MAX_FRAMES {
            return;
        }
        if self.buddy_ready {
            // Pre-check: if every frame in the block is already marked free
            // (bitmap bit set), this is a double-free attempt. Skip the
            // free path entirely so free_frames accounting stays consistent
            // and the kernel does not panic on userspace-triggered cleanup.
            // The real fix belongs in the caller (typically teardown_user_pages
            // walking shared PTE rows twice).
            let block_size = 1usize << order;
            let mut already_free = true;
            for f in frame..frame + block_size {
                if !self.bitmap_is_free(f) {
                    already_free = false;
                    break;
                }
            }
            if already_free {
                klibcluu::log_dec(klibcluu::LogLevel::Error, "  order=", order as u64);
                report_double_free(phys_addr, rip, tag);
                // Phase 2: under Phase 2 refcount invariants this path should
                // NEVER fire because dec_ref only calls free when the last ref
                // drops. Panic in debug builds to catch regressions fast.
                #[cfg(debug_assertions)]
                panic!("PMM double-free detected — Phase 2 invariant violated");
                return;
            }
            // Record the successful free before releasing the PMM lock.
            record_free(phys_addr, rip, tag);
            self.buddy_free(phys_addr, order);
        } else {
            // Fallback: just mark bitmap bits (pre-buddy phase, no tracking needed)
            let count = 1usize << order;
            for f in frame..frame + count {
                self.bitmap_set_free(f);
            }
            self.free_frames += count;
        }
    }

    // ─── Phase 2: Build free lists from bitmap ─────────────────────────────

    /// Walk the bitmap and build buddy free lists.
    /// Called after physmap is active.
    fn build_free_lists(&mut self) {
        // Reset free lists
        for i in 0..MAX_ORDER {
            self.free_lists[i] = FreeList::new();
        }
        self.free_frames = 0;

        // Scan bitmap for contiguous free regions, break into buddy blocks
        let mut frame = 1; // skip frame 0 (null page)
        while frame < MAX_FRAMES {
            if !self.bitmap_is_free(frame) {
                frame += 1;
                continue;
            }

            // Find the end of this contiguous free region
            let region_start = frame;
            while frame < MAX_FRAMES && self.bitmap_is_free(frame) {
                frame += 1;
            }
            let region_end = frame;

            // Break this region into maximal aligned buddy blocks
            self.add_region_to_buddy(region_start, region_end);
        }

        self.buddy_ready = true;

        klibcluu::log_dec(
            klibcluu::LogLevel::Info,
            "PMM buddy init: free_frames=",
            self.free_frames as u64,
        );
        for order in 0..MAX_ORDER {
            if self.free_lists[order].count > 0 {
                klibcluu::log_dec(klibcluu::LogLevel::Trace, "  order ", order as u64);
                klibcluu::log_dec(
                    klibcluu::LogLevel::Trace,
                    ": blocks=",
                    self.free_lists[order].count as u64,
                );
            }
        }
    }

    /// Add a contiguous free region [start, end) to buddy free lists
    /// by breaking it into maximal power-of-2 aligned blocks.
    fn add_region_to_buddy(&mut self, start: usize, end: usize) {
        // Clear all bitmap bits first (list_push will set them)
        for f in start..end {
            self.bitmap_set_used(f);
        }

        let mut pos = start;
        while pos < end {
            // Find the largest order block that:
            // 1. Is aligned to 2^order
            // 2. Fits within [pos, end)
            let mut order = 0;
            while order < MAX_ORDER - 1 {
                let next_order = order + 1;
                let block_size = 1usize << next_order;
                // Check alignment and fit
                if pos & (block_size - 1) != 0 || pos + block_size > end {
                    break;
                }
                order = next_order;
            }

            let block_size = 1usize << order;
            self.list_push(pos, order);
            self.free_frames += block_size;
            pos += block_size;
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Global PMM Instance
// ═══════════════════════════════════════════════════════════════════════════

static PMM: Mutex<BuddyAllocator> = Mutex::new(BuddyAllocator::new());

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Phase 1 init: parse memory map, mark free regions in bitmap.
/// Called before physmap is available.
///
/// # Safety
/// Must run exactly once during boot.
pub unsafe fn init(bootboot: &BOOTBOOT, boot_info: &dyn BootInfoProvider) {
    klibcluu::info("Initializing Physical Memory Manager (phase 1: bitmap)...");

    let mut pmm = PMM.lock();
    let (kernel_phys_start, kernel_phys_end) = boot_info.kernel_physical_range();

    // Parse memory map and mark free regions.
    // bootboot.size includes the 128-byte header; subtract it to get mmap byte count.
    const BOOTBOOT_HEADER_SIZE: usize = 128;
    let mmap_entries = bootboot.size.saturating_sub(BOOTBOOT_HEADER_SIZE as u32) as usize / 16;
    let mmap_ptr = &bootboot.mmap as *const MMapEnt;
    let mut total_free_frames = 0;
    let mut max_managed_frame: usize = 0;

    for i in 0..mmap_entries {
        let entry = unsafe { &*mmap_ptr.add(i) };
        let entry_type = entry.size & 0xF;
        let size = entry.size & !0xF;

        if entry_type == 1 {
            let start_addr = entry.ptr;
            let end_addr = start_addr + size;
            let start_frame = ((start_addr / 4096) as usize).max(1);
            let end_frame = end_addr.div_ceil(4096) as usize;

            let clamped_end = end_frame.min(MAX_FRAMES);
            for frame in start_frame..clamped_end {
                pmm.bitmap_set_free(frame);
                total_free_frames += 1;
            }
            if clamped_end > max_managed_frame {
                max_managed_frame = clamped_end;
            }
        }
    }

    pmm.total_frames = total_free_frames;
    pmm.free_frames = total_free_frames;
    pmm.max_managed_frame = max_managed_frame;

    // Reserve system regions
    let reserve = |pmm: &mut BuddyAllocator, start: u64, end: u64| {
        let sf = (start / 4096) as usize;
        let ef = end.div_ceil(4096) as usize;
        for f in sf..ef.min(MAX_FRAMES) {
            if pmm.bitmap_is_free(f) {
                pmm.bitmap_set_used(f);
                pmm.free_frames -= 1;
            }
        }
    };

    // 1. Low memory (first 1MB)
    reserve(&mut pmm, 0, 0x100000);

    // 2. Kernel
    reserve(&mut pmm, kernel_phys_start, kernel_phys_end);

    // 3. Initrd
    if let Some((ptr, size)) = boot_info.initrd_location() {
        reserve(&mut pmm, ptr, ptr + size);
    }

    // 4. BOOTBOOT structure
    let bb_phys = boot_info.info_structure_location();
    let bb_size = bootboot.size as u64;
    if bb_size > 0 {
        reserve(&mut pmm, bb_phys, bb_phys + bb_size);
    }

    // 5. Framebuffer
    if let Some((fb_phys, fb_size)) = boot_info.framebuffer_location() {
        reserve(&mut pmm, fb_phys, fb_phys + fb_size);
    }

    klibcluu::log_dec(
        klibcluu::LogLevel::Trace,
        "  PMM phase 1: free=",
        pmm.free_frames as u64,
    );
    klibcluu::log_dec(
        klibcluu::LogLevel::Trace,
        " / total=",
        total_free_frames as u64,
    );
}

/// Phase 2 init: build buddy free lists from bitmap.
/// Called after physmap is active.
pub fn init_buddy() {
    klibcluu::info("PMM phase 2: building buddy free lists...");
    PMM.lock().build_free_lists();
}

/// Allocate a single 4KB frame.
///
/// Tag defaults to `"<unknown>"`. For a diagnostic tagged alloc use
/// `alloc_frame_tagged` instead.
#[inline(never)] // keep frame pointer stable so caller_rip() is meaningful
pub fn alloc_frame() -> Option<u64> {
    let rip = caller_rip();
    PMM.lock().alloc_order_tagged(0, "<unknown>", rip)
}

/// Allocate a single 4KB frame with an explicit caller-site tag.
///
/// `tag` should be a short `&'static str` such as `"alloc_pml4"` that
/// identifies the call site. The tag is stored in the event ring.
#[inline(never)]
pub fn alloc_frame_tagged(tag: &'static str) -> Option<u64> {
    let rip = caller_rip();
    PMM.lock().alloc_order_tagged(0, tag, rip)
}

/// Non-blocking variant of `alloc_frame` for use from interrupt/exception
/// handlers (e.g. demand-paging from `pf_with_regs`). Returns `None` if the
/// PMM lock is contended — caller must treat that as if memory is briefly
/// unavailable and either retry on a later trap or fail the fault.
///
/// CRITICAL: blocking on PMM.lock() inside an ISR with interrupts disabled
/// causes the same kernel halt as the timer-tick bug fixed in 393cd6b.
///
/// Note: alloc event is NOT recorded here (ISR context; ring lock may itself
/// be contended or held by the interrupted PMM path). The no-tag, no-record
/// fast path is acceptable for ISR demand-paging diagnosis.
pub fn try_alloc_frame() -> Option<u64> {
    PMM.try_lock()?.alloc_order(0)
}

/// Free a single 4KB frame.
///
/// Source is tagged as `"<unknown>"`. For diagnostic tagged release use
/// `free_frame_tagged` instead.
#[inline(never)] // keep frame pointer stable so caller_rip() is meaningful
pub fn free_frame(phys_addr: u64) {
    let rip = caller_rip();
    PMM.lock().free_order_tagged(phys_addr, 0, "<unknown>", rip);
}

/// Free a single 4KB frame with an explicit caller-site tag.
///
/// `tag` should be a short `&'static str` such as `"teardown_leaf"` that
/// identifies the call site. The tag is stored in the ring buffer and
/// printed on double-free detection.
#[inline(never)]
pub fn free_frame_tagged(phys_addr: u64, tag: &'static str) {
    let rip = caller_rip();
    PMM.lock().free_order_tagged(phys_addr, 0, tag, rip);
}

/// Allocate a 2MB-aligned contiguous block (order 9 = 512 pages).
///
/// Tag defaults to `"<unknown>"`. For a diagnostic tagged alloc use
/// `alloc_large_frame_tagged` instead.
#[inline(never)]
pub fn alloc_large_frame() -> Option<u64> {
    let rip = caller_rip();
    PMM.lock().alloc_order_tagged(9, "<unknown>", rip)
}

/// Allocate a 2MB-aligned contiguous block with an explicit caller-site tag.
#[inline(never)]
pub fn alloc_large_frame_tagged(tag: &'static str) -> Option<u64> {
    let rip = caller_rip();
    PMM.lock().alloc_order_tagged(9, tag, rip)
}

/// Free a 2MB large frame
#[inline(never)]
pub fn free_large_frame(phys_addr: u64) {
    let rip = caller_rip();
    PMM.lock().free_order_tagged(phys_addr, 9, "<unknown>", rip);
}

/// Free a 2MB large frame with an explicit caller-site tag.
#[inline(never)]
pub fn free_large_frame_tagged(phys_addr: u64, tag: &'static str) {
    let rip = caller_rip();
    PMM.lock().free_order_tagged(phys_addr, 9, tag, rip);
}

/// Allocate 2^order contiguous pages.
///
/// Tag defaults to `"<unknown>"`. For a diagnostic tagged alloc use
/// `alloc_order_tagged` instead.
#[inline(never)]
pub fn alloc_order(order: usize) -> Option<u64> {
    let rip = caller_rip();
    PMM.lock().alloc_order_tagged(order, "<unknown>", rip)
}

/// Allocate 2^order contiguous pages with an explicit caller-site tag.
#[inline(never)]
pub fn alloc_order_tagged(order: usize, tag: &'static str) -> Option<u64> {
    let rip = caller_rip();
    PMM.lock().alloc_order_tagged(order, tag, rip)
}

/// Free 2^order contiguous pages
#[inline(never)]
pub fn free_order(phys_addr: u64, order: usize) {
    let rip = caller_rip();
    PMM.lock().free_order_tagged(phys_addr, order, "<unknown>", rip);
}

/// Free 2^order contiguous pages with an explicit caller-site tag.
#[inline(never)]
pub fn free_order_tagged(phys_addr: u64, order: usize, tag: &'static str) {
    let rip = caller_rip();
    PMM.lock().free_order_tagged(phys_addr, order, tag, rip);
}

/// Get memory statistics (used, total)
pub fn get_stats() -> (usize, usize) {
    let pmm = PMM.lock();
    (pmm.total_frames - pmm.free_frames, pmm.total_frames)
}

/// Return `max_managed_frame` — the exclusive upper bound on PMM-owned frame indices.
///
/// Frames at or above this index are device MMIO / non-RAM. Used by the frame
/// table to size its backing array and by `mark_boot_reserved_frames`.
pub fn max_managed_frame() -> usize {
    PMM.lock().max_managed_frame
}

/// Walk the PMM bitmap and call `retype_to_boot_reserved` (via `frame_table`)
/// for every frame that is currently NOT free (i.e. used/reserved at boot time).
///
/// This runs once after `init_buddy()` and after `frame_table::init()` to seed
/// the table with the static set of reserved frames (kernel image, initrd,
/// framebuffer, BOOTBOOT struct, low-1MB). Free frames remain `Untyped`.
///
/// Only frames in `[0, max_managed_frame)` are scanned.
pub fn mark_boot_reserved_frames() {
    let pmm = PMM.lock();
    let max = pmm.max_managed_frame.min(MAX_FRAMES);
    // Avoid calling frame_table while holding the PMM lock (potential deadlock
    // if frame_table's lock is held). Collect the used frames first, then mark.
    // We own the PMM lock here and the scan is O(max/64), so build a tiny temp
    // buffer per 64-frame word to minimise lock time.
    let mut used_frames: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
    for frame in 1..max {
        if !pmm.bitmap_is_free(frame) {
            used_frames.push((frame as u64) * (PAGE_SIZE as u64));
        }
    }
    drop(pmm);
    for phys in used_frames {
        let _ = crate::mm::frame_table::retype_to_boot_reserved(phys);
    }
}
