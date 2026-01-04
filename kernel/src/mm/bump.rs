//! Bootstrap Bump Allocator
//!
//! A simple bump allocator used during early kernel initialization before
//! the full memory management system is set up.
//!
//! # Limitations
//!
//! - No deallocation support (memory is never freed)
//! - Fixed size (4 MB static buffer)
//! - Not suitable for long-term use
//!
//! # Usage
//!
//! This allocator is used during boot to:
//! 1. Parse BOOTBOOT memory map (needs Vec)
//! 2. Initialize BuddyAllocator (needs Vec for free lists)
//! 3. Set up initial page tables
//!
//! After initialization completes, we'll replace this with a proper
//! heap allocator (linked_list_allocator) backed by the BuddyAllocator.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Size of bootstrap heap (512 KB)
///
/// This is enough for:
/// - BOOTBOOT memory map parsing (~few KB)
/// - BuddyAllocator free lists (MAX_ORDER + 1 = 13 Vecs)
/// - Initial page table structures (~100-200 KB max)
const HEAP_SIZE: usize = 512 * 1024;

/// Static heap buffer for bump allocator
///
/// This is placed in the .bss section and zeroed by the bootloader.
#[repr(align(16))]
struct HeapBuffer([u8; HEAP_SIZE]);

static mut HEAP: HeapBuffer = HeapBuffer([0; HEAP_SIZE]);

/// Current allocation offset (next free byte)
static HEAP_OFFSET: AtomicUsize = AtomicUsize::new(0);

/// Bootstrap bump allocator
///
/// This is a simple allocator that never frees memory. It just bumps a pointer
/// forward for each allocation.
pub struct BumpAllocator;

impl BumpAllocator {
    /// Get heap usage statistics
    ///
    /// Returns (used_bytes, total_bytes)
    pub fn stats() -> (usize, usize) {
        let used = HEAP_OFFSET.load(Ordering::Relaxed);
        (used, HEAP_SIZE)
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        // Load current offset
        let mut offset = HEAP_OFFSET.load(Ordering::Relaxed);

        loop {
            // Align offset up to required alignment
            let aligned_offset = (offset + align - 1) & !(align - 1);

            // Calculate new offset after this allocation
            let new_offset = aligned_offset + size;

            // Check if we have space
            if new_offset > HEAP_SIZE {
                klibcluu::error("Bootstrap heap exhausted!");
                klibcluu::log_dec(klibcluu::LogLevel::Error, "  Requested: ", size as u64);
                klibcluu::log_dec(klibcluu::LogLevel::Error, "  Used: ", offset as u64);
                klibcluu::log_dec(klibcluu::LogLevel::Error, "  Total: ", HEAP_SIZE as u64);
                return null_mut();
            }

            // Try to reserve this range atomically
            match HEAP_OFFSET.compare_exchange(
                offset,
                new_offset,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Success! Return pointer to our allocation
                    let ptr = unsafe {
                        let heap_base = HEAP.0.as_ptr() as usize;
                        (heap_base + aligned_offset) as *mut u8
                    };

                    // Log large allocations for debugging
                    if klibcluu::logger::should_log(klibcluu::LogLevel::Trace) && size > 1024 {
                        klibcluu::log_dec(klibcluu::LogLevel::Trace, "Bootstrap alloc: ", size as u64);
                        klibcluu::log_hex(klibcluu::LogLevel::Trace, "  bytes at 0x", ptr as u64);
                    }

                    return ptr;
                }
                Err(actual_offset) => {
                    // Someone else allocated, retry with new offset
                    offset = actual_offset;
                }
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator doesn't support deallocation
        // This is fine for bootstrap phase - we never free memory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bump_allocator_basic() {
        let allocator = BumpAllocator;

        // Allocate some memory
        unsafe {
            let layout = Layout::from_size_align(64, 8).unwrap();
            let ptr1 = allocator.alloc(layout);
            assert!(!ptr1.is_null());

            let ptr2 = allocator.alloc(layout);
            assert!(!ptr2.is_null());

            // Pointers should be different
            assert_ne!(ptr1, ptr2);
        }
    }

    #[test]
    fn test_alignment() {
        let allocator = BumpAllocator;

        unsafe {
            // Request 16-byte aligned allocation
            let layout = Layout::from_size_align(32, 16).unwrap();
            let ptr = allocator.alloc(layout);

            // Check alignment
            assert_eq!(ptr as usize % 16, 0);
        }
    }
}
