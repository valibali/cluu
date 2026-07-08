//! Memory-related syscall stubs.

use super::{c_int, c_void, off_t, size_t};
use crate::errno::{set_errno, EBADF, EINVAL, ENOMEM, ENOSYS};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

extern crate alloc;
use alloc::vec::Vec;

/// Start of dynamic heap region for newlib's _sbrk.
/// In c-runtime mode, the Rust allocator delegates to malloc, so _sbrk
/// owns the full heap range starting at USER_HEAP_START.
const HEAP_START: usize = 0x0080_0000;

/// Maximum heap address.
const HEAP_MAX: usize = 0x4000_0000;

/// Page size.
const PAGE_SIZE: usize = 4096;

/// M6 ASLR: per-process random offset for _sbrk heap start. Bounded to
/// 128 MB (page-aligned), same range as the Rust allocator's heap ASLR.
const HEAP_ASLR_RANGE: usize = 128 * 1024 * 1024;
static HEAP_BRK: AtomicUsize = AtomicUsize::new(0);

fn randomized_heap_start() -> usize {
    let start = HEAP_BRK.load(Ordering::Relaxed);
    if start != 0 {
        return start;
    }
    let mut buf = [0u8; 8];
    klibcluu::crypto::fill_random(&mut buf);
    let r = u64::from_le_bytes(buf) as usize;
    let offset = (r & (HEAP_ASLR_RANGE - 1)) & !0xFFF;
    let randomized = HEAP_START + offset;
    HEAP_BRK.store(randomized, Ordering::Relaxed);
    randomized
}

// ═══════════════════════════════════════════════════════════════════════════
// mmap region: 0x4100_0000 .. 0x5000_0000 (240 MB)
// ═══════════════════════════════════════════════════════════════════════════

const MMAP_REGION_START: usize = 0x4100_0000;
const MMAP_REGION_END: usize = 0x5000_0000;

// M6 ASLR: per-process random offset added to MMAP_REGION_START. Bounded
// to 128 MB (page-aligned) so the mmap region stays below MMAP_REGION_END.
const MMAP_ASLR_RANGE: usize = 128 * 1024 * 1024;
static MMAP_START_RANDOMIZED: AtomicUsize = AtomicUsize::new(0);

fn randomized_mmap_start() -> usize {
    let start = MMAP_START_RANDOMIZED.load(Ordering::Relaxed);
    if start != 0 {
        return start;
    }
    let mut buf = [0u8; 8];
    klibcluu::crypto::fill_random(&mut buf);
    let r = u64::from_le_bytes(buf) as usize;
    let offset = (r & (MMAP_ASLR_RANGE - 1)) & !0xFFF;
    let randomized = MMAP_REGION_START + offset;
    MMAP_START_RANDOMIZED.store(randomized, Ordering::Relaxed);
    randomized
}

/// Next free address in the mmap region (bump allocator).
/// Allocation uses first-fit over tracked regions so freed holes are reused.

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
    entries: Vec<MmapRegion>,
}

impl MmapRegionTable {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn insert(&mut self, region: MmapRegion) -> bool {
        if self.entries.try_reserve(1).is_err() {
            return false;
        }
        self.entries.push(region);
        true
    }

    fn remove(&mut self, addr: usize) -> Option<MmapRegion> {
        let idx = self.entries.iter().position(|r| r.addr == addr)?;
        Some(self.entries.swap_remove(idx))
    }

    fn find_exact(&self, addr: usize, len: usize) -> Option<MmapRegion> {
        self.entries
            .iter()
            .find(|r| r.addr == addr && r.len == len)
            .copied()
    }

    fn update_prot_exact(&mut self, addr: usize, len: usize, prot: c_int) -> bool {
        for r in self.entries.iter_mut() {
            if r.addr == addr && r.len == len {
                r.prot = prot;
                return true;
            }
        }
        false
    }

    fn overlaps(&self, start: usize, end: usize) -> bool {
        self.entries.iter().any(|r| {
            let r_end = r.addr.saturating_add(r.len);
            start < r_end && r.addr < end
        })
    }

    fn find_first_fit(&self, len: usize) -> Option<usize> {
        let mut cursor = randomized_mmap_start();
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
/// Supports `MAP_ANONYMOUS` and `MAP_PRIVATE` file-backed mappings.
/// `MAP_SHARED` file-backed mappings return `ENOSYS`.
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
    offset: off_t,
) -> *mut c_void {
    if length == 0 {
        set_errno(EINVAL);
        return MAP_FAILED;
    }

    // Determine mapping type
    let is_anonymous = (flags & MAP_ANONYMOUS) != 0 || fd == -1;
    let is_file_backed = !is_anonymous;

    // MAP_SHARED with file descriptor: detect /dev/fb0 by probing for the FB
    // header magic; if found, route to MAP_DEVICE_WC. All other cases return ENOSYS.
    if is_file_backed && (flags & MAP_SHARED) != 0 {
        use super::file::{_lseek, _read, SEEK_CUR, SEEK_SET};
        const FB_HEADER_MAGIC: u32 = 0x4642_4630;

        let mut hdr = [0u8; 40];
        let saved_pos = _lseek(fd, 0, SEEK_CUR);
        if saved_pos >= 0 && _lseek(fd, 0, SEEK_SET) == 0 {
            let n = _read(fd, hdr.as_mut_ptr() as *mut c_void, 40);
            let _ = _lseek(fd, saved_pos, SEEK_SET); // best-effort restore
            if n == 40 {
                let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
                if magic == FB_HEADER_MAGIC {
                    let fb_size = u64::from_le_bytes([
                        hdr[24], hdr[25], hdr[26], hdr[27],
                        hdr[28], hdr[29], hdr[30], hdr[31],
                    ]) as usize;
                    let fb_phys = u64::from_le_bytes([
                        hdr[32], hdr[33], hdr[34], hdr[35],
                        hdr[36], hdr[37], hdr[38], hdr[39],
                    ]);
                    if length > fb_size {
                        set_errno(EINVAL);
                        return MAP_FAILED;
                    }
                    if offset != 0 {
                        set_errno(EINVAL);
                        return MAP_FAILED;
                    }
                    let aligned_len = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
                    let num_pages = aligned_len / PAGE_SIZE;
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
                    let space_token = crate::boot::space_token();
                    if space_token == 0 {
                        set_errno(ENOMEM);
                        return MAP_FAILED;
                    }
                    let mut kern_flags: usize = 0;
                    if prot & PROT_READ  != 0 { kern_flags |= 0x01; }
                    if prot & PROT_WRITE != 0 { kern_flags |= 0x02; }
                    if prot & PROT_EXEC  != 0 { kern_flags |= 0x04; }
                    if kern_flags == 0 { kern_flags = 0x01; }
                    kern_flags |= crate::syscall::MAP_DEVICE_WC;
                    match crate::syscall::space_map_range(
                        space_token, virt_addr, fb_phys as usize, kern_flags, num_pages, 0,
                    ) {
                        Ok(_) => {}
                        Err(_) => {
                            set_errno(ENOMEM);
                            return MAP_FAILED;
                        }
                    }
                    let region = MmapRegion { addr: virt_addr, len: aligned_len, prot };
                    if !MMAP_REGIONS.lock().insert(region) {
                        let _ = crate::syscall::space_unmap(space_token, virt_addr, num_pages);
                        set_errno(ENOMEM);
                        return MAP_FAILED;
                    }
                    return virt_addr as *mut c_void;
                }
            }
        }
        set_errno(ENOSYS);
        return MAP_FAILED;
    }

    // MAP_SHARED | MAP_ANONYMOUS with a page-aligned non-zero offset: treat
    // offset as a source virtual address in the caller's own space and route
    // to MAP_SHARE_PHYS, creating a read-only alias of the caller's pages at
    // a new virtual address in the mmap region. This is the userspace wrapper
    // around the kernel's MAP_SHARE_PHYS flag (handlers.rs MAP_SHARE_PHYS
    // = 0x800). The kernel remaps the caller's physical frames backing
    // `offset` into the space identified by `space_token` at `virt_addr`,
    // always read-only (writable bit ignored).
    //
    // Cross-process sharing: the owner maps a writable anonymous region
    // (offset=0), then calls space_map_range directly with the RECEIVER's
    // space token + MAP_SHARE_PHYS + the owner's source VA to map those
    // frames read-only into the receiver. This mmap path covers the same-space
    // alias case; the cross-process case uses space_map_range directly since
    // mmap only holds the caller's own space token. See
    // doc/book/memory_model.md (mmap region section).
    if is_anonymous && (flags & MAP_SHARED) != 0 && offset != 0 {
        let src_virt = offset as usize;
        if src_virt & (PAGE_SIZE - 1) != 0 {
            set_errno(EINVAL);
            return MAP_FAILED;
        }
        let aligned_len = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let num_pages = aligned_len / PAGE_SIZE;

        // MAP_SHARE_PHYS mappings are always read-only; drop PROT_WRITE so
        // the tracked prot reflects the actual mapping.
        let effective_prot = prot & !PROT_WRITE;
        let mut kern_flags: usize = 0;
        if effective_prot & PROT_READ != 0 {
            kern_flags |= 0x01;
        }
        if effective_prot & PROT_EXEC != 0 {
            kern_flags |= 0x04;
        }
        if kern_flags == 0 {
            kern_flags = 0x01;
        }
        kern_flags |= crate::syscall::MAP_SHARE_PHYS;

        let space_token = crate::boot::space_token();
        if space_token == 0 {
            set_errno(ENOMEM);
            return MAP_FAILED;
        }

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

        // data_len must be non-zero for MAP_SHARE_PHYS (kernel validation);
        // pass aligned_len to satisfy the check. The kernel does not copy
        // data for MAP_SHARE_PHYS — it remaps physical frames.
        match crate::syscall::space_map_range(
            space_token,
            virt_addr,
            src_virt,
            kern_flags,
            num_pages,
            aligned_len,
        ) {
            Ok(_) => {}
            Err(_) => {
                set_errno(ENOMEM);
                return MAP_FAILED;
            }
        }

        let region = MmapRegion {
            addr: virt_addr,
            len: aligned_len,
            prot: effective_prot,
        };
        if !MMAP_REGIONS.lock().insert(region) {
            let _ = crate::syscall::space_unmap(space_token, virt_addr, num_pages);
            set_errno(ENOMEM);
            return MAP_FAILED;
        }
        return virt_addr as *mut c_void;
    }

    // Validate offset alignment for file-backed mappings
    if is_file_backed && (offset as usize) & (PAGE_SIZE - 1) != 0 {
        set_errno(EINVAL);
        return MAP_FAILED;
    }

    // Validate fd capabilities: must be readable and seekable
    if is_file_backed {
        let table = crate::fd_table::FD_TABLE.lock();
        match table.get(fd) {
            Some(entry) if entry.is_readable() && entry.is_seekable() => {}
            _ => {
                set_errno(EBADF);
                return MAP_FAILED;
            }
        }
        // Lock dropped here — must NOT hold across file I/O below
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

    // For file-backed mappings, populate pages from the file
    if is_file_backed {
        use super::file::{_lseek, _read, SEEK_CUR, SEEK_SET};

        // Save current fd position
        let saved_pos = _lseek(fd, 0, SEEK_CUR);
        if saved_pos < 0 {
            // Non-seekable fd — shouldn't happen (validated above), but be safe.
            // Region is tracked; caller can munmap. Return the valid zero-filled mapping.
            return virt_addr as *mut c_void;
        }

        // Seek to requested offset
        if _lseek(fd, offset, SEEK_SET) < 0 {
            let _ = _lseek(fd, saved_pos, SEEK_SET);
            return virt_addr as *mut c_void;
        }

        // Read file data into mapped pages in a loop.
        // _read may return < requested (VFS grant buffer is 64KB).
        let mut total_read: usize = 0;
        let target = length; // original length, not aligned_len
        while total_read < target {
            let remaining = target - total_read;
            let ptr = (virt_addr + total_read) as *mut c_void;
            let n = _read(fd, ptr, remaining);
            if n <= 0 {
                break; // EOF or error — remaining bytes stay zero
            }
            total_read += n as usize;
        }

        // Restore original fd position
        let _ = _lseek(fd, saved_pos, SEEK_SET);
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

// mremap flags
pub const MREMAP_MAYMOVE: c_int = 0x1;
pub const MREMAP_DONTUNMAP: c_int = 0x2;

/// Resize an existing mmap mapping in place, or relocate it if growth is
/// blocked and `MREMAP_MAYMOVE` is set.
///
/// Composes existing `space_unmap` + `space_map_range` — no new InvokeOp.
///
/// # Arguments
/// - `old_address`: Start of the existing mapping (page-aligned)
/// - `old_size`: Current size (must match tracked region's aligned length)
/// - `new_size`: Requested new size (rounded up to page boundary)
/// - `flags`: `MREMAP_MAYMOVE` to allow relocation when in-place growth fails
///
/// # Returns
/// Pointer to the resized mapping, or `MAP_FAILED` (-1) on error.
///
/// # Errors
/// - `EINVAL`: `old_address` is NULL, unaligned, or not a tracked region;
///   `old_size` mismatches the tracked region; `new_size` is 0; flags invalid.
/// - `ENOMEM`: no space to grow/relocate, or space token unavailable.
///
/// # Limitations vs Linux mremap
/// - `MREMAP_DONTUNMAP` is accepted but treated as a no-op flag (the old
///   range is always unmapped after relocation — no hole-punching).
/// - `MREMAP_FIXED` (5th-arg `new_address`) is not supported.
#[no_mangle]
pub extern "C" fn mremap(
    old_address: *mut c_void,
    old_size: size_t,
    new_size: size_t,
    flags: c_int,
) -> *mut c_void {
    _mremap(old_address, old_size, new_size, flags)
}

#[no_mangle]
pub extern "C" fn _mremap(
    old_address: *mut c_void,
    old_size: size_t,
    new_size: size_t,
    flags: c_int,
) -> *mut c_void {
    // ── Input validation (before any syscall) ─────────────────────────────
    if old_address.is_null() {
        set_errno(EINVAL);
        return MAP_FAILED;
    }
    let old_addr = old_address as usize;
    if old_addr & (PAGE_SIZE - 1) != 0 {
        set_errno(EINVAL);
        return MAP_FAILED;
    }
    if old_size == 0 || new_size == 0 {
        set_errno(EINVAL);
        return MAP_FAILED;
    }
    if flags & !(MREMAP_MAYMOVE | MREMAP_DONTUNMAP) != 0 {
        set_errno(EINVAL);
        return MAP_FAILED;
    }

    let old_aligned = (old_size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let new_aligned = (new_size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let old_pages = old_aligned / PAGE_SIZE;
    let new_pages = new_aligned / PAGE_SIZE;

    let space_token = crate::boot::space_token();
    if space_token == 0 {
        set_errno(ENOMEM);
        return MAP_FAILED;
    }

    // ── Lookup tracked region (exact addr + len match) ────────────────────
    let region = {
        let table = MMAP_REGIONS.lock();
        match table.find_exact(old_addr, old_aligned) {
            Some(r) => r,
            None => {
                set_errno(EINVAL);
                return MAP_FAILED;
            }
        }
    };

    if new_aligned == old_aligned {
        return old_address;
    }

    let mut kern_flags: usize = 0;
    if region.prot & PROT_READ != 0 {
        kern_flags |= 0x01;
    }
    if region.prot & PROT_WRITE != 0 {
        kern_flags |= 0x02;
    }
    if region.prot & PROT_EXEC != 0 {
        kern_flags |= 0x04;
    }
    if kern_flags == 0 {
        kern_flags = 0x01;
    }

    // ── Shrink: unmap trailing pages ──────────────────────────────────────
    if new_aligned < old_aligned {
        let pages_to_unmap = old_pages - new_pages;
        let unmap_start = old_addr + new_aligned;
        if crate::syscall::space_unmap(space_token, unmap_start, pages_to_unmap).is_err() {
            set_errno(ENOMEM);
            return MAP_FAILED;
        }
        let mut table = MMAP_REGIONS.lock();
        let _ = table.remove(old_addr);
        let _ = table.insert(MmapRegion {
            addr: old_addr,
            len: new_aligned,
            prot: region.prot,
        });
        return old_address;
    }

    // ── Grow: try in-place extension first ────────────────────────────────
    let extra_pages = new_pages - old_pages;
    let new_end = old_addr + new_aligned;

    // Check whether the extension range is free (excluding the region itself).
    let can_extend = {
        let mut table = MMAP_REGIONS.lock();
        let _ = table.remove(old_addr);
        let free = !table.overlaps(old_addr + old_aligned, new_end);
        let _ = table.insert(MmapRegion {
            addr: old_addr,
            len: old_aligned,
            prot: region.prot,
        });
        free
    };

    if can_extend {
        let map_start = old_addr + old_aligned;
        match crate::syscall::space_map_range(space_token, map_start, 0, kern_flags, extra_pages, 0)
        {
            Ok(_) | Err(crate::Error::AlreadyExists) => {
                let mut table = MMAP_REGIONS.lock();
                let _ = table.remove(old_addr);
                let _ = table.insert(MmapRegion {
                    addr: old_addr,
                    len: new_aligned,
                    prot: region.prot,
                });
                return old_address;
            }
            Err(_) => { /* fall through to MAYMOVE */ }
        }
    }

    if (flags & MREMAP_MAYMOVE) == 0 {
        set_errno(ENOMEM);
        return MAP_FAILED;
    }

    // ── Relocate: find new spot, map, copy, unmap old ─────────────────────
    let new_addr = {
        let table = MMAP_REGIONS.lock();
        match table.find_first_fit(new_aligned) {
            Some(a) => a,
            None => {
                set_errno(ENOMEM);
                return MAP_FAILED;
            }
        }
    };

    match crate::syscall::space_map_range(space_token, new_addr, 0, kern_flags, new_pages, 0) {
        Ok(_) | Err(crate::Error::AlreadyExists) => {}
        Err(_) => {
            set_errno(ENOMEM);
            return MAP_FAILED;
        }
    }

    // SAFETY: both ranges are mapped in the current address space —
    // old_addr..old_addr+old_aligned (existing region) and
    // new_addr..new_addr+new_aligned (just mapped). They do not overlap
    // because find_first_fit avoids existing regions.
    let copy_bytes = core::cmp::min(old_size, new_size);
    unsafe {
        core::ptr::copy_nonoverlapping(
            old_addr as *const u8,
            new_addr as *mut u8,
            copy_bytes,
        );
    }

    let _ = crate::syscall::space_unmap(space_token, old_addr, old_pages);

    {
        let mut table = MMAP_REGIONS.lock();
        let _ = table.remove(old_addr);
        let _ = table.insert(MmapRegion {
            addr: new_addr,
            len: new_aligned,
            prot: region.prot,
        });
    }

    new_addr as *mut c_void
}

/// msync — no-op for MAP_PRIVATE mappings.
#[no_mangle]
pub extern "C" fn msync(_addr: *mut c_void, _length: size_t, _flags: c_int) -> c_int {
    0
}

#[cfg(test)]
fn reset_mmap_state_for_tests() {
    let mut table = MMAP_REGIONS.lock();
    *table = MmapRegionTable::new();
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
    if HEAP_BRK.load(Ordering::SeqCst) == 0 {
        randomized_heap_start();
    }
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
    if !(HEAP_START..=HEAP_MAX).contains(&new_brk) {
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
        let old_page = old_brk.div_ceil(PAGE_SIZE);
        let new_page = new_brk.div_ceil(PAGE_SIZE);

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
    let brk = HEAP_BRK.load(Ordering::SeqCst);
    if brk == 0 {
        randomized_heap_start()
    } else {
        brk
    }
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

    if !(HEAP_START..=HEAP_MAX).contains(&target) {
        set_errno(ENOMEM);
        return -1;
    }

    if HEAP_BRK.load(Ordering::SeqCst) == 0 {
        randomized_heap_start();
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

    #[test]
    fn mmap_region_table_grows_beyond_64() {
        reset_mmap_state_for_tests();
        let mut table = MMAP_REGIONS.lock();
        for i in 0..100 {
            let addr = MMAP_REGION_START + i * PAGE_SIZE;
            assert!(
                table.insert(MmapRegion {
                    addr,
                    len: PAGE_SIZE,
                    prot: PROT_READ,
                }),
                "insert failed at region {} (addr={:#x})",
                i,
                addr,
            );
        }
        let last_addr = MMAP_REGION_START + 99 * PAGE_SIZE;
        assert_eq!(
            table.find_exact(last_addr, PAGE_SIZE).unwrap().addr,
            last_addr,
        );
    }

    #[test]
    fn find_first_fit_returns_none_when_region_exhausted() {
        reset_mmap_state_for_tests();
        let table = MMAP_REGIONS.lock();
        let huge_len = (MMAP_REGION_END - MMAP_REGION_START) + PAGE_SIZE;
        assert_eq!(table.find_first_fit(huge_len), None);
    }

    #[test]
    fn remove_then_reinsert_does_not_corrupt() {
        reset_mmap_state_for_tests();
        let mut table = MMAP_REGIONS.lock();
        for i in 0..10 {
            let addr = MMAP_REGION_START + i * PAGE_SIZE;
            assert!(table.insert(MmapRegion {
                addr,
                len: PAGE_SIZE,
                prot: PROT_READ,
            }));
        }
        let mid_addr = MMAP_REGION_START + 5 * PAGE_SIZE;
        assert!(table.remove(mid_addr).is_some());
        assert!(table.find_exact(mid_addr, PAGE_SIZE).is_none());
        for i in 0..10 {
            let addr = MMAP_REGION_START + i * PAGE_SIZE;
            if i == 5 {
                continue;
            }
            assert!(table.find_exact(addr, PAGE_SIZE).is_some(), "missing addr={:#x}", addr);
        }
    }

    // ── mremap validation tests (return before any syscall) ───────────────

    #[test]
    fn mremap_null_addr_returns_einval() {
        reset_mmap_state_for_tests();
        let r = _mremap(core::ptr::null_mut(), PAGE_SIZE, PAGE_SIZE * 2, 0);
        assert_eq!(r, MAP_FAILED);
        assert_eq!(crate::errno::errno(), EINVAL);
    }

    #[test]
    fn mremap_unaligned_addr_returns_einval() {
        reset_mmap_state_for_tests();
        let r = _mremap(0x4100_0001 as *mut c_void, PAGE_SIZE, PAGE_SIZE * 2, 0);
        assert_eq!(r, MAP_FAILED);
        assert_eq!(crate::errno::errno(), EINVAL);
    }

    #[test]
    fn mremap_zero_old_size_returns_einval() {
        reset_mmap_state_for_tests();
        let r = _mremap(MMAP_REGION_START as *mut c_void, 0, PAGE_SIZE * 2, 0);
        assert_eq!(r, MAP_FAILED);
        assert_eq!(crate::errno::errno(), EINVAL);
    }

    #[test]
    fn mremap_zero_new_size_returns_einval() {
        reset_mmap_state_for_tests();
        let r = _mremap(MMAP_REGION_START as *mut c_void, PAGE_SIZE, 0, 0);
        assert_eq!(r, MAP_FAILED);
        assert_eq!(crate::errno::errno(), EINVAL);
    }

    #[test]
    fn mremap_invalid_flags_returns_einval() {
        reset_mmap_state_for_tests();
        let r = _mremap(
            MMAP_REGION_START as *mut c_void,
            PAGE_SIZE,
            PAGE_SIZE * 2,
            0x80,
        );
        assert_eq!(r, MAP_FAILED);
        assert_eq!(crate::errno::errno(), EINVAL);
    }

    #[test]
    fn mremap_invalid_addr_no_panic() {
        reset_mmap_state_for_tests();
        let r = _mremap(0xDEAD_0000 as *mut c_void, PAGE_SIZE, PAGE_SIZE * 2, MREMAP_MAYMOVE);
        assert_eq!(r, MAP_FAILED);
    }

    // ── mremap table-level resize logic (no syscalls) ─────────────────────
    // These verify the MmapRegionTable state transitions that mremap performs:
    // grow-in-place, shrink, and relocate.

    #[test]
    fn mremap_table_grow_in_place() {
        reset_mmap_state_for_tests();
        let mut table = MMAP_REGIONS.lock();
        let addr = MMAP_REGION_START;
        let old_len = PAGE_SIZE;
        let new_len = PAGE_SIZE * 2;
        assert!(table.insert(MmapRegion {
            addr,
            len: old_len,
            prot: PROT_READ | PROT_WRITE,
        }));

        let region = table.find_exact(addr, old_len).unwrap();
        let _ = table.remove(addr);
        let _ = table.insert(MmapRegion {
            addr,
            len: new_len,
            prot: region.prot,
        });

        let resized = table.find_exact(addr, new_len).unwrap();
        assert_eq!(resized.addr, addr);
        assert_eq!(resized.len, new_len);
        assert_eq!(resized.prot, PROT_READ | PROT_WRITE);
        assert!(table.find_exact(addr, old_len).is_none());
    }

    #[test]
    fn mremap_table_shrink() {
        reset_mmap_state_for_tests();
        let mut table = MMAP_REGIONS.lock();
        let addr = MMAP_REGION_START;
        let old_len = PAGE_SIZE * 4;
        let new_len = PAGE_SIZE * 2;
        assert!(table.insert(MmapRegion {
            addr,
            len: old_len,
            prot: PROT_READ,
        }));

        let region = table.find_exact(addr, old_len).unwrap();
        let _ = table.remove(addr);
        let _ = table.insert(MmapRegion {
            addr,
            len: new_len,
            prot: region.prot,
        });

        let resized = table.find_exact(addr, new_len).unwrap();
        assert_eq!(resized.len, new_len);
        assert!(table.find_exact(addr, old_len).is_none());

        // The freed tail [addr+new_len .. addr+old_len] should be reusable.
        let tail = table.find_first_fit(PAGE_SIZE * 2);
        assert_eq!(tail, Some(addr + new_len));
    }

    #[test]
    fn mremap_table_relocate() {
        reset_mmap_state_for_tests();
        let mut table = MMAP_REGIONS.lock();
        let old_addr = MMAP_REGION_START;
        let old_len = PAGE_SIZE;
        let new_len = PAGE_SIZE * 3;

        // Block in-place growth: place a region right after old_addr.
        assert!(table.insert(MmapRegion {
            addr: old_addr + old_len,
            len: PAGE_SIZE,
            prot: PROT_READ,
        }));
        assert!(table.insert(MmapRegion {
            addr: old_addr,
            len: old_len,
            prot: PROT_READ,
        }));

        // Simulate MAYMOVE: find a new spot, update tracking.
        let new_addr = table.find_first_fit(new_len).expect("free slot exists");
        assert_ne!(new_addr, old_addr);
        let _ = table.remove(old_addr);
        let _ = table.insert(MmapRegion {
            addr: new_addr,
            len: new_len,
            prot: PROT_READ,
        });

        assert!(table.find_exact(new_addr, new_len).is_some());
        assert!(table.find_exact(old_addr, old_len).is_none());
    }
}
