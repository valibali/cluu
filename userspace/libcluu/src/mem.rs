//! Memory constants and utilities
//!
//! Central definitions for page sizes and alignment functions.

// ═══════════════════════════════════════════════════════════════════════════
// Memory Constants
// ═══════════════════════════════════════════════════════════════════════════

/// 4KB page size (standard x86_64 page)
pub const PAGE_SIZE: usize = 4096;
/// Bits to shift for 4KB page offset
pub const PAGE_SHIFT: u32 = 12;
/// Mask for 4KB page offset
pub const PAGE_MASK: usize = PAGE_SIZE - 1;

/// 2MB large page size
pub const LARGE_PAGE_SIZE: usize = 2 * 1024 * 1024;
/// Bits to shift for 2MB page offset
pub const LARGE_PAGE_SHIFT: u32 = 21;
/// Mask for 2MB page offset
pub const LARGE_PAGE_MASK: usize = LARGE_PAGE_SIZE - 1;

/// 1GB huge page size (for future use)
pub const HUGE_PAGE_SIZE: usize = 1024 * 1024 * 1024;
/// Bits to shift for 1GB page offset
pub const HUGE_PAGE_SHIFT: u32 = 30;

/// Number of 4KB pages in a 2MB large page
pub const PAGES_PER_LARGE_PAGE: usize = 512;

// ═══════════════════════════════════════════════════════════════════════════
// Alignment Functions
// ═══════════════════════════════════════════════════════════════════════════

/// Align value up to alignment
#[inline]
pub const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// Align value down to alignment
#[inline]
pub const fn align_down(value: usize, align: usize) -> usize {
    value & !(align - 1)
}

/// Check if value is aligned
#[inline]
pub const fn is_aligned(value: usize, align: usize) -> bool {
    value & (align - 1) == 0
}

/// Align to 4KB page boundary (up)
#[inline]
pub const fn page_align_up(value: usize) -> usize {
    align_up(value, PAGE_SIZE)
}

/// Align to 4KB page boundary (down)
#[inline]
pub const fn page_align_down(value: usize) -> usize {
    align_down(value, PAGE_SIZE)
}

/// Align to 2MB large page boundary (up)
#[inline]
pub const fn large_page_align_up(value: usize) -> usize {
    align_up(value, LARGE_PAGE_SIZE)
}

/// Align to 2MB large page boundary (down)
#[inline]
pub const fn large_page_align_down(value: usize) -> usize {
    align_down(value, LARGE_PAGE_SIZE)
}

/// Check if address is 2MB aligned
#[inline]
pub const fn is_large_page_aligned(value: usize) -> bool {
    is_aligned(value, LARGE_PAGE_SIZE)
}

/// Calculate number of 4KB pages needed for a given size
#[inline]
pub const fn pages_for_size(size: usize) -> usize {
    (size + PAGE_SIZE - 1) / PAGE_SIZE
}

/// Calculate number of 2MB large pages needed for a given size
#[inline]
pub const fn large_pages_for_size(size: usize) -> usize {
    (size + LARGE_PAGE_SIZE - 1) / LARGE_PAGE_SIZE
}
