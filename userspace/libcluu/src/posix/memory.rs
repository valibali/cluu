//! Memory-related syscall stubs.

use super::c_void;
use crate::errno::{set_errno, ENOMEM};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Start of dynamic heap region.
const HEAP_START: usize = 0x0080_0000;

/// Maximum heap address.
const HEAP_MAX: usize = 0x4000_0000;

/// Page size.
const PAGE_SIZE: usize = 4096;

/// Current heap break (end of allocated heap).
static HEAP_BRK: AtomicUsize = AtomicUsize::new(HEAP_START);

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
