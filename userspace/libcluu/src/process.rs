//! Helpers for loading ELF segments and stacks into a new address space.

use crate::ipc::{extract_reply_id, reply};
use crate::mem::PAGE_SIZE;
use crate::syscall::{
    endpoint_create, space_create, space_destroy, space_map_range, space_protect_unmapped,
    space_unmap, MAP_GUARD, MAP_SHARE_PHYS,
};
use crate::types::{IpcFlags, Message};
use crate::{
    elf::{ElfFile, LoadableSegment},
    Error, Result,
};
use alloc::vec::Vec;

/// Stack canary word planted at the bottom of each process stack
/// (at `stack_base + 8`, just above the guard page). procmgr reads
/// this word on thread exit; a mismatch indicates the stack grew
/// past its bottom and clobbered the canary before hitting the guard
/// page.
pub const STACK_CANARY: u64 = 0xDEADBEEF_CAFE_BABE;

pub fn map_segments(space_token: usize, elf: &ElfFile, bytes: &[u8]) -> Result<()> {
    // set_text_with_source records a single text region per space, so only
    // one segment can be M9 demand-paged. Subsequent text segments are
    // eagerly mapped to avoid overwriting the recorded text region.
    let mut text_demand_paged = false;
    for segment in elf.segments_iter() {
        map_segment(space_token, segment, bytes, &mut text_demand_paged)?;
    }
    Ok(())
}

fn map_segment(
    space_token: usize,
    segment: &LoadableSegment,
    bytes: &[u8],
    text_demand_paged: &mut bool,
) -> Result<()> {
    let vaddr = segment.vaddr as usize;
    let mem_size = segment.mem_size as usize;
    if mem_size == 0 {
        return Ok(());
    }

    let file_offset = segment.file_offset as usize;
    let file_size = segment.file_size as usize;
    if file_offset + file_size > bytes.len() {
        return Err(Error::InvalidArgument);
    }

    // M9: demand-page text segments (executable + read-only). Install
    // not-present PTEs — no physical frame is allocated until first
    // execution, reducing boot memory. .data/.bss stay eagerly mapped.
    // Source bytes are copied into a kernel heap buffer at install time
    // (invoke_space_protect PROTECT_INSTALL_UNMAPPED), so the fault
    // handler never needs to translate the source space's page table at
    // fault time. Only executable, non-writable segments are demand-paged
    // — `set_text_with_source` records a single text region per space, so
    // a later writable segment must not overwrite it.
    let demand_page_text =
        !*text_demand_paged && segment.is_executable() && !segment.is_writable();
    if demand_page_text {
        *text_demand_paged = true;
    }

    // Handle non-page-aligned segments (e.g., .bss after .tdata).
    // The first partial page was already mapped by the previous segment,
    // so skip it and only map from the next page boundary onward.
    let page_offset = vaddr & (PAGE_SIZE - 1);
    if page_offset != 0 {
        let next_page = (vaddr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let end = vaddr + mem_size;
        if end <= next_page {
            // Entire segment fits within the already-mapped page — nothing to do.
            return Ok(());
        }
        // Map pages from next_page to end.
        let remaining = end - next_page;
        let num_pages = remaining.div_ceil(PAGE_SIZE);
        // Adjust file data: skip the bytes that fall on the already-mapped page.
        let skip = next_page - vaddr;
        let adj_file_size = file_size.saturating_sub(skip);
        let adj_file_offset = file_offset + file_size - adj_file_size;

        if demand_page_text {
            let source_ptr = bytes.as_ptr() as usize + adj_file_offset;
            return space_protect_unmapped(
                space_token,
                next_page,
                num_pages,
                adj_file_size,
                source_ptr,
            )
            .map(|_| ());
        }

        let slice = &bytes[adj_file_offset..adj_file_offset + adj_file_size];
        return space_map_range(
            space_token,
            next_page,
            slice.as_ptr() as usize,
            segment.page_flags() as usize,
            num_pages,
            adj_file_size,
        )
        .map(|_| ());
    }

    // Page-aligned case (common path).
    let num_pages = mem_size.div_ceil(PAGE_SIZE);

    if demand_page_text {
        let source_ptr = bytes.as_ptr() as usize + file_offset;
        return space_protect_unmapped(
            space_token,
            vaddr,
            num_pages,
            file_size,
            source_ptr,
        )
        .map(|_| ());
    }

    let slice = &bytes[file_offset..file_offset + file_size];

    space_map_range(
        space_token,
        vaddr,
        slice.as_ptr() as usize,
        segment.page_flags() as usize,
        num_pages,
        file_size,
    )?;

    Ok(())
}

pub fn map_stack(
    space_token: usize,
    stack_top: usize,
    stack_size: usize,
    flags: usize,
) -> Result<()> {
    map_stack_with_guard(space_token, stack_top, stack_size, flags, 0)
}

/// Map a stack with `guard_pages` not-present guard pages below the stack base.
///
/// The guard pages are installed with `MAP_GUARD` (present=false, no physical
/// frame). Any access into the guard region triggers a page fault that kills
/// the faulting thread (or forwards to a registered fault_endpoint).
/// `guard_pages = 0` produces the same behavior as `map_stack`.
///
/// A stack canary (`STACK_CANARY`) is planted at `stack_base + 8` — the
/// lowest 8 bytes of usable stack, just above the guard page. procmgr reads
/// it on thread exit to detect stack-bottom overflow that didn't quite reach
/// the guard page.
pub fn map_stack_with_guard(
    space_token: usize,
    stack_top: usize,
    stack_size: usize,
    flags: usize,
    guard_pages: usize,
) -> Result<()> {
    let stack_base = stack_top - stack_size;
    let num_pages = stack_size / PAGE_SIZE;

    if guard_pages > 0 {
        let guard_base = stack_base - guard_pages * PAGE_SIZE;
        space_map_range(
            space_token,
            guard_base,
            0,
            MAP_GUARD,
            guard_pages,
            0,
        )?;
    }

    if num_pages == 0 {
        return Ok(());
    }

    // First page carries the canary at offset 8 (bytes 0..7 are zero).
    let mut canary_page = [0u8; 16];
    canary_page[8..16].copy_from_slice(&STACK_CANARY.to_le_bytes());
    space_map_range(
        space_token,
        stack_base,
        canary_page.as_ptr() as usize,
        flags,
        1,
        canary_page.len(),
    )?;

    if num_pages > 1 {
        space_map_range(
            space_token,
            stack_base + PAGE_SIZE,
            0,
            flags,
            num_pages - 1,
            0,
        )?;
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// M8: Copy-on-write fork primitive
// ═══════════════════════════════════════════════════════════════════════════

/// Fault message label used by the kernel's `try_forward_fault` path.
/// Matches `PROCMGR_FAULT_LABEL` in `libs/procmgr-common/src/labels.rs`.
pub const COW_FAULT_LABEL: u32 = 0xFA017;

/// Fault reply label values understood by the kernel's `handle_fault_reply`.
/// Label `0` → RESUME (restore saved context, re-execute faulting instruction).
/// Non-zero label → KILL (terminate the faulted thread).
pub const COW_REPLY_RESUME: u32 = 0;
pub const COW_REPLY_KILL: u32 = 1;

/// Page-fault type discriminant in `FaultType` (kernel/src/sched/thread.rs).
/// Only `PageFault = 0` is relevant to COW; other fault types are fatal.
pub const COW_FAULT_TYPE_PAGE_FAULT: usize = 0;

/// Error-code bit: fault caused by write access (x86 #PF error code bit 1).
pub const COW_PF_WRITE: usize = 0x2;

/// A region in the parent's address space to share into the child as
/// copy-on-write. The parent's physical frames are mapped read-only in the
/// child via `MAP_SHARE_PHYS`; the first write in the child faults and is
/// resolved by `cow_handle_fault` with a private copy.
#[derive(Clone, Copy)]
pub struct CowRegion {
    /// Virtual address in the parent (caller) where the source pages live.
    /// Must be page-aligned.
    pub parent_virt: usize,
    /// Virtual address in the child where the pages should appear.
    /// Must be page-aligned.
    pub child_virt: usize,
    /// Number of 4 KiB pages to share.
    pub num_pages: usize,
}

/// Result of `cow_fork`: the child address space, the fault endpoint, and the
/// recorded parent→child mappings used by `cow_handle_fault` to resolve
/// write faults.
pub struct CowFork {
    /// Token for the child address space. Bears SPACE_MAP + THREAD_CONTROL +
    /// DESTROY — caller uses it to map pages, create the child thread, and
    /// destroy the space when done.
    pub child_space_token: usize,
    /// Token for the fault endpoint. The caller recv's COW fault messages
    /// here and passes them to `cow_handle_fault`. Also passed to
    /// `thread_set_fault_endpoint` on the child thread.
    pub fault_endpoint_token: usize,
    /// `(child_virt, parent_virt, num_pages)` for each shared region, used by
    /// `cow_handle_fault` to find the source page to copy on a write fault.
    pub mappings: Vec<(usize, usize, usize)>,
}

/// Create a copy-on-write fork: a new address space with the given parent
/// regions shared read-only. Writes in the child fault and are resolved by
/// `cow_handle_fault` with a private copy, leaving the parent's pages intact.
///
/// Composes existing invoke ops only — no new syscalls:
///   `space_create` + `endpoint_create` + `space_map_range(MAP_SHARE_PHYS)`.
///
/// The caller is responsible for:
/// 1. `thread_create(child_space_token, entry, stack, …, START_SUSPENDED)`
/// 2. `thread_set_fault_endpoint(child_thread_token, fault_endpoint_token)`
/// 3. `thread_resume(child_thread_token)`
/// 4. Loop: `recv(fault_endpoint_token, &mut msg, …)` → `cow_handle_fault(&fork, &msg)`
///
/// `authority_token` must bear the `CREATE` right (TOKEN_SELF or root).
pub fn cow_fork(authority_token: usize, regions: &[CowRegion]) -> Result<CowFork> {
    let child_space_token = space_create(authority_token)?;
    let fault_endpoint_token = endpoint_create(authority_token)?;

    let mut mappings = Vec::with_capacity(regions.len());
    for region in regions {
        if region.parent_virt & (PAGE_SIZE - 1) != 0
            || region.child_virt & (PAGE_SIZE - 1) != 0
            || region.num_pages == 0
        {
            return Err(Error::InvalidArgument);
        }
        // MAP_SHARE_PHYS: kernel translates parent_virt in the CALLER's page
        // table and maps the same physical frame read-only in the child.
        // data_len must be non-zero (kernel validation); pass the region size.
        let data_len = region.num_pages * PAGE_SIZE;
        space_map_range(
            child_space_token,
            region.child_virt,
            region.parent_virt,
            MAP_SHARE_PHYS,
            region.num_pages,
            data_len,
        )?;
        mappings.push((region.child_virt, region.parent_virt, region.num_pages));
    }

    Ok(CowFork {
        child_space_token,
        fault_endpoint_token,
        mappings,
    })
}

/// Handle one COW fault message received on the fault endpoint.
///
/// If the fault is a write-fault inside a shared COW region: unmap the
/// shared read-only page in the child, allocate a fresh writable frame
/// with a copy of the parent's page contents, and reply RESUME so the
/// kernel re-executes the faulting instruction in the child.
///
/// If the fault is outside all COW regions, not a page-fault, or not a
/// write-fault: reply KILL so the kernel terminates the child thread,
/// leaving the parent unaffected.
///
/// Returns `Ok(true)` if a COW fault was handled (child will resume),
/// `Ok(false)` if the fault was fatal (child killed). Returns `Err` on
/// IPC or syscall failure during handling.
pub fn cow_handle_fault(fork: &CowFork, msg: &Message) -> Result<bool> {
    let fault_type = msg.words[0];
    let fault_addr = msg.words[1];
    let error_code = msg.words[2];

    let reply_id = extract_reply_id(msg).ok_or(Error::InvalidArgument)?;

    // Only COW-write page-faults are handled. Anything else (read fault on
    // unmapped page, non-page-fault, instruction-fetch fault) is fatal.
    let is_cow_fault = fault_type == COW_FAULT_TYPE_PAGE_FAULT
        && (error_code & COW_PF_WRITE) != 0;
    if !is_cow_fault {
        let kill = Message::new(COW_REPLY_KILL, [0; 6], 0);
        let _ = reply(reply_id, &kill, IpcFlags::empty());
        return Ok(false);
    }

    // Locate the COW region containing the faulting page.
    let page = fault_addr & !(PAGE_SIZE - 1);
    let mut found = None;
    for &(child_virt, parent_virt, num_pages) in &fork.mappings {
        let region_end = child_virt + num_pages * PAGE_SIZE;
        if page >= child_virt && page < region_end {
            found = Some((child_virt, parent_virt));
            break;
        }
    }

    let (child_virt_base, parent_virt_base) = match found {
        Some(r) => r,
        None => {
            let kill = Message::new(COW_REPLY_KILL, [0; 6], 0);
            let _ = reply(reply_id, &kill, IpcFlags::empty());
            return Ok(false);
        }
    };

    // COW resolution: unmap the shared read-only page, then map a fresh
    // writable copy with the parent's page contents. space_unmap dec_refs
    // the shared frame; since the parent still maps it, the frame survives.
    // space_map_range allocates a new frame, copies `parent_page`'s 4 KiB
    // into it (translating parent_page in the caller's = parent's CR3), and
    // maps it writable in the child.
    let offset = page - child_virt_base;
    let parent_page = parent_virt_base + offset;

    space_unmap(fork.child_space_token, page, 1)?;

    space_map_range(
        fork.child_space_token,
        page,
        parent_page,
        0x02, // writable (read + write)
        1,
        PAGE_SIZE,
    )?;

    let resume = Message::new(COW_REPLY_RESUME, [0; 6], 0);
    reply(reply_id, &resume, IpcFlags::empty())?;
    Ok(true)
}

/// Destroy a `CowFork`: kill the child thread (if still alive) and tear down
/// the child address space. The fault endpoint token is NOT revoked here —
/// the caller owns it and may reuse or revoke it separately.
///
/// `child_thread_token` is the token returned by `thread_create` for the
/// child thread. Pass `0` if the thread was never created or is already dead.
pub fn cow_destroy(fork: &CowFork, child_thread_token: usize) -> Result<()> {
    if child_thread_token != 0 {
        let _ = crate::syscall::thread_destroy(child_thread_token);
    }
    space_destroy(fork.child_space_token)?;
    Ok(())
}
