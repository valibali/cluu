//! Memory-related syscall stubs.

use super::{c_int, c_void, off_t, size_t};
use crate::errno::{set_errno, EINVAL, ENOMEM, ENOSYS};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

extern crate alloc;

/// Start of dynamic heap region for newlib's _sbrk.
/// In c-runtime mode, the Rust allocator delegates to malloc, so _sbrk
/// owns the full heap range starting at USER_HEAP_START.
const HEAP_START: usize = 0x0080_0000;

/// Maximum heap address.
const HEAP_MAX: usize = 0x4000_0000;

/// Page size.
const PAGE_SIZE: usize = 4096;

/// Current heap break (end of allocated heap).
static HEAP_BRK: AtomicUsize = AtomicUsize::new(0x0080_0000);

// ═══════════════════════════════════════════════════════════════════════════
// mmap region: 0x4100_0000 .. 0x5000_0000 (240 MB)
// ═══════════════════════════════════════════════════════════════════════════

const MMAP_REGION_START: usize = 0x4100_0000;
const MMAP_REGION_END: usize = 0x5000_0000;

/// Next free address in the mmap region (bump allocator).
// Allocation now uses first-fit over tracked regions so freed holes are reused.

/// Maximum tracked mmap regions.
const MAX_MMAP_REGIONS: usize = 64;

/// A tracked mmap allocation.
#[derive(Clone, Copy)]
struct MmapRegion {
    addr: usize,
    len: usize,
    prot: c_int,
}

/// Active mmap allocations.
static MMAP_REGIONS: Mutex<MmapRegionTable> = Mutex::new(MmapRegionTable::new());

struct MmapRegionTable {
    entries: [Option<MmapRegion>; MAX_MMAP_REGIONS],
}

impl MmapRegionTable {
    const fn new() -> Self {
        Self {
            entries: [None; MAX_MMAP_REGIONS],
        }
    }

    fn insert(&mut self, region: MmapRegion) -> bool {
        for slot in self.entries.iter_mut() {
            if slot.is_none() {
                *slot = Some(region);
                return true;
            }
        }
        false
    }

    fn remove(&mut self, addr: usize) -> Option<MmapRegion> {
        for slot in self.entries.iter_mut() {
            if let Some(r) = slot {
                if r.addr == addr {
                    return slot.take();
                }
            }
        }
        None
    }

    fn find_exact(&self, addr: usize, len: usize) -> Option<MmapRegion> {
        for slot in self.entries.iter() {
            if let Some(r) = slot {
                if r.addr == addr && r.len == len {
                    return Some(*r);
                }
            }
        }
        None
    }

    fn update_prot_exact(&mut self, addr: usize, len: usize, prot: c_int) -> bool {
        for slot in self.entries.iter_mut() {
            if let Some(r) = slot {
                if r.addr == addr && r.len == len {
                    r.prot = prot;
                    return true;
                }
            }
        }
        false
    }

    fn overlaps(&self, start: usize, end: usize) -> bool {
        for slot in self.entries.iter() {
            if let Some(r) = slot {
                let r_end = r.addr.saturating_add(r.len);
                if start < r_end && r.addr < end {
                    return true;
                }
            }
        }
        false
    }

    fn find_first_fit(&self, len: usize) -> Option<usize> {
        let mut cursor = MMAP_REGION_START;
        while cursor <= MMAP_REGION_END.saturating_sub(len) {
            let end = cursor + len;
            if !self.overlaps(cursor, end) {
                return Some(cursor);
            }
            cursor = cursor.saturating_add(PAGE_SIZE);
        }
        None
    }
}

// POSIX mmap flags
pub const MAP_SHARED: c_int = 0x01;
pub const MAP_PRIVATE: c_int = 0x02;
pub const MAP_FIXED: c_int = 0x10;
pub const MAP_ANONYMOUS: c_int = 0x20;
pub const MAP_ANON: c_int = MAP_ANONYMOUS;

// POSIX mmap protections
pub const PROT_NONE: c_int = 0x0;
pub const PROT_READ: c_int = 0x1;
pub const PROT_WRITE: c_int = 0x2;
pub const PROT_EXEC: c_int = 0x4;

/// MAP_FAILED sentinel.
const MAP_FAILED: *mut c_void = (-1isize) as *mut c_void;

/// Map pages into the calling process's address space.
///
/// Supports `MAP_ANONYMOUS` mappings (not backed by a file).
/// File-backed mappings return `ENOSYS`.
///
/// # Arguments
/// - `addr`: Hint address (ignored unless MAP_FIXED)
/// - `length`: Size of mapping in bytes (rounded up to page boundary)
/// - `prot`: Protection flags (PROT_READ, PROT_WRITE, PROT_EXEC)
/// - `flags`: Mapping flags (MAP_ANONYMOUS, MAP_PRIVATE, MAP_SHARED, MAP_FIXED)
/// - `fd`: File descriptor (-1 for anonymous)
/// - `offset`: Offset in file (ignored for anonymous)
///
/// # Returns
/// Pointer to mapped region, or MAP_FAILED (-1) on error.
#[no_mangle]
pub extern "C" fn mmap(
    addr: *mut c_void,
    length: size_t,
    prot: c_int,
    flags: c_int,
    fd: c_int,
    offset: off_t,
) -> *mut c_void {
    _mmap(addr, length, prot, flags, fd, offset)
}

#[no_mangle]
pub extern "C" fn _mmap(
    addr: *mut c_void,
    length: size_t,
    prot: c_int,
    flags: c_int,
    fd: c_int,
    _offset: off_t,
) -> *mut c_void {
    if length == 0 {
        set_errno(EINVAL);
        return MAP_FAILED;
    }

    // Only anonymous mappings supported
    if (flags & MAP_ANONYMOUS) == 0 || fd != -1 {
        set_errno(ENOSYS);
        return MAP_FAILED;
    }

    // Round length up to page boundary
    let aligned_len = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let num_pages = aligned_len / PAGE_SIZE;

    // Convert prot flags to kernel flags
    let mut kern_flags: usize = 0;
    if prot & PROT_READ != 0 {
        kern_flags |= 0x01;
    }
    if prot & PROT_WRITE != 0 {
        kern_flags |= 0x02;
    }
    if prot & PROT_EXEC != 0 {
        kern_flags |= 0x04;
    }
    // Default: at least readable
    if kern_flags == 0 {
        kern_flags = 0x01;
    }

    let space_token = crate::boot::space_token();
    if space_token == 0 {
        set_errno(ENOMEM);
        return MAP_FAILED;
    }

    // Determine virtual address
    let virt_addr = if (flags & MAP_FIXED) != 0 && !addr.is_null() {
        let a = addr as usize;
        if a & (PAGE_SIZE - 1) != 0 {
            set_errno(EINVAL);
            return MAP_FAILED;
        }
        let end = a.saturating_add(aligned_len);
        if a < MMAP_REGION_START || end > MMAP_REGION_END {
            set_errno(EINVAL);
            return MAP_FAILED;
        }
        if MMAP_REGIONS.lock().overlaps(a, end) {
            set_errno(EINVAL);
            return MAP_FAILED;
        }
        a
    } else {
        let Some(fit) = MMAP_REGIONS.lock().find_first_fit(aligned_len) else {
            set_errno(ENOMEM);
            return MAP_FAILED;
        };
        fit
    };

    // Map pages via kernel
    match crate::syscall::space_map_range(space_token, virt_addr, 0, kern_flags, num_pages, 0) {
        Ok(_) | Err(crate::Error::AlreadyExists) => {}
        Err(_) => {
            set_errno(ENOMEM);
            return MAP_FAILED;
        }
    }

    // Track the region for munmap
    let region = MmapRegion {
        addr: virt_addr,
        len: aligned_len,
        prot,
    };
    if !MMAP_REGIONS.lock().insert(region) {
        // Rollback mapping if local bookkeeping is exhausted.
        let _ = crate::syscall::space_unmap(space_token, virt_addr, num_pages);
        set_errno(ENOMEM);
        return MAP_FAILED;
    }

    virt_addr as *mut c_void
}

/// Unmap pages from the calling process's address space.
///
/// # Arguments
/// - `addr`: Start address (must be page-aligned, from a previous mmap)
/// - `length`: Size to unmap (rounded up to page boundary)
///
/// # Returns
/// 0 on success, -1 on error.
#[no_mangle]
pub extern "C" fn munmap(addr: *mut c_void, length: size_t) -> c_int {
    _munmap(addr, length)
}

#[no_mangle]
pub extern "C" fn _munmap(addr: *mut c_void, length: size_t) -> c_int {
    if addr.is_null() || length == 0 {
        set_errno(EINVAL);
        return -1;
    }

    let virt = addr as usize;
    if virt & (PAGE_SIZE - 1) != 0 {
        set_errno(EINVAL);
        return -1;
    }

    let aligned_len = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let num_pages = aligned_len / PAGE_SIZE;

    // Unmap via kernel
    let space_token = crate::boot::space_token();
    if space_token == 0 {
        set_errno(EINVAL);
        return -1;
    }

    // Require exact region tracking for now to keep lifecycle strict.
    if MMAP_REGIONS.lock().find_exact(virt, aligned_len).is_none() {
        set_errno(EINVAL);
        return -1;
    }

    match crate::syscall::space_unmap(space_token, virt, num_pages) {
        Ok(()) => {
            let _ = MMAP_REGIONS.lock().remove(virt);
            0
        }
        Err(_) => {
            set_errno(ENOMEM);
            -1
        }
    }
}

/// Change protection on a tracked mmap region.
///
/// Current limitation: kernel PTE retagging is not exposed yet, so this updates
/// userspace bookkeeping only after strict validation.
#[no_mangle]
pub extern "C" fn mprotect(addr: *mut c_void, len: size_t, prot: c_int) -> c_int {
    if addr.is_null() || len == 0 {
        set_errno(EINVAL);
        return -1;
    }

    let virt = addr as usize;
    if virt & (PAGE_SIZE - 1) != 0 {
        set_errno(EINVAL);
        return -1;
    }
    if prot & !(PROT_NONE | PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        set_errno(EINVAL);
        return -1;
    }

    let aligned_len = (len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let mut kern_flags: usize = 0;
    if prot & PROT_READ != 0 {
        kern_flags |= 0x01;
    }
    if prot & PROT_WRITE != 0 {
        kern_flags |= 0x02;
    }
    if prot & PROT_EXEC != 0 {
        kern_flags |= 0x04;
    }
    if prot == PROT_NONE {
        // x86 user mappings do not currently support true PROT_NONE without guard-page support.
        set_errno(ENOSYS);
        return -1;
    }

    let space_token = crate::boot::space_token();
    if space_token == 0 {
        set_errno(EINVAL);
        return -1;
    }
    let num_pages = aligned_len / PAGE_SIZE;
    if crate::syscall::space_protect(space_token, virt, num_pages, kern_flags).is_err() {
        set_errno(EINVAL);
        return -1;
    }

    if MMAP_REGIONS
        .lock()
        .update_prot_exact(virt, aligned_len, prot)
    {
        return 0;
    }
    set_errno(EINVAL);
    -1
}

#[cfg(test)]
fn reset_mmap_state_for_tests() {
    let mut table = MMAP_REGIONS.lock();
    *table = MmapRegionTable::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_fit_reuses_freed_slot() {
        reset_mmap_state_for_tests();
        let mut table = MMAP_REGIONS.lock();
        assert!(table.insert(MmapRegion {
            addr: MMAP_REGION_START,
            len: PAGE_SIZE,
            prot: PROT_READ,
        }));
        assert!(table.insert(MmapRegion {
            addr: MMAP_REGION_START + PAGE_SIZE * 2,
            len: PAGE_SIZE,
            prot: PROT_READ,
        }));
        let _ = table.remove(MMAP_REGION_START);
        assert_eq!(table.find_first_fit(PAGE_SIZE), Some(MMAP_REGION_START));
    }

    #[test]
    fn update_prot_requires_exact_region() {
        reset_mmap_state_for_tests();
        let mut table = MMAP_REGIONS.lock();
        assert!(table.insert(MmapRegion {
            addr: MMAP_REGION_START,
            len: PAGE_SIZE * 2,
            prot: PROT_READ,
        }));
        assert!(table.update_prot_exact(MMAP_REGION_START, PAGE_SIZE * 2, PROT_READ | PROT_WRITE));
        assert!(!table.update_prot_exact(MMAP_REGION_START, PAGE_SIZE, PROT_EXEC));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// sbrk / brk (heap management)
// ═══════════════════════════════════════════════════════════════════════════

/// Expand or contract the heap.
///
/// This is the classic Unix `sbrk()` function used by malloc implementations.
///
/// # Arguments
/// - `increment`: Number of bytes to add (positive) or remove (negative)
///
/// # Returns
/// Previous heap break on success, `(void*)-1` on error (errno set).
///
/// # Notes
///
/// - Positive increment: Expands heap, mapping new pages if needed
/// - Zero increment: Returns current heap break without modification
/// - Negative increment: Contracts heap (pages not unmapped, just tracked)
#[no_mangle]
pub extern "C" fn _sbrk(increment: isize) -> *mut c_void {
    let old_brk = HEAP_BRK.load(Ordering::SeqCst);

    if increment == 0 {
        return old_brk as *mut c_void;
    }

    let new_brk = if increment > 0 {
        old_brk.saturating_add(increment as usize)
    } else {
        old_brk.saturating_sub((-increment) as usize)
    };

    // Check bounds
    if new_brk < HEAP_START || new_brk > HEAP_MAX {
        set_errno(ENOMEM);
        return (-1isize) as *mut c_void;
    }

    // If expanding, map new pages
    if new_brk > old_brk {
        let space_token = crate::boot::space_token();
        if space_token == 0 {
            set_errno(ENOMEM);
            return (-1isize) as *mut c_void;
        }

        // Calculate pages needed
        let old_page = (old_brk + PAGE_SIZE - 1) / PAGE_SIZE;
        let new_page = (new_brk + PAGE_SIZE - 1) / PAGE_SIZE;

        if new_page > old_page {
            let pages_needed = new_page - old_page;
            let map_start = old_page * PAGE_SIZE;

            // Map new pages (zero-filled, read+write)
            match crate::syscall::space_map_range(
                space_token,
                map_start,
                0,    // source_ptr = 0 for zero-fill
                0x03, // flags: read + write
                pages_needed,
                0, // data_len = 0
            ) {
                Ok(_) | Err(crate::Error::AlreadyExists) => {}
                Err(_) => {
                    set_errno(ENOMEM);
                    return (-1isize) as *mut c_void;
                }
            }
        }
    }

    // Update heap break
    HEAP_BRK.store(new_brk, Ordering::SeqCst);

    old_brk as *mut c_void
}

/// Get current heap break.
///
/// Equivalent to `_sbrk(0)`.
#[inline]
pub fn current_brk() -> usize {
    HEAP_BRK.load(Ordering::SeqCst)
}

/// Set heap break to a specific address.
///
/// Like `brk()` syscall - sets absolute heap end.
///
/// # Arguments
/// - `addr`: New heap break address
///
/// # Returns
/// 0 on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn brk(addr: *mut c_void) -> i32 {
    let target = addr as usize;

    if target < HEAP_START || target > HEAP_MAX {
        set_errno(ENOMEM);
        return -1;
    }

    let current = HEAP_BRK.load(Ordering::SeqCst);
    let increment = target as isize - current as isize;

    let result = _sbrk(increment);
    if result == (-1isize) as *mut c_void {
        -1
    } else {
        0
    }
}
