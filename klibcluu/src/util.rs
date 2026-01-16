//! Common utility functions and memory constants

// ═══════════════════════════════════════════════════════════════════════════
// Memory Constants
// ═══════════════════════════════════════════════════════════════════════════

/// 4KB page size (standard x86_64 page)
pub const PAGE_SIZE: u64 = 4096;
/// 4KB page size as usize
pub const PAGE_SIZE_USIZE: usize = 4096;
/// Bits to shift for 4KB page offset
pub const PAGE_SHIFT: u32 = 12;
/// Mask for 4KB page offset
pub const PAGE_MASK: u64 = PAGE_SIZE - 1;

/// 2MB large page size
pub const LARGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;
/// 2MB large page size as usize
pub const LARGE_PAGE_SIZE_USIZE: usize = 2 * 1024 * 1024;
/// Bits to shift for 2MB page offset
pub const LARGE_PAGE_SHIFT: u32 = 21;
/// Mask for 2MB page offset
pub const LARGE_PAGE_MASK: u64 = LARGE_PAGE_SIZE - 1;

/// 1GB huge page size (for future use)
pub const HUGE_PAGE_SIZE: u64 = 1024 * 1024 * 1024;
/// Bits to shift for 1GB page offset
pub const HUGE_PAGE_SHIFT: u32 = 30;

/// Number of 4KB pages in a 2MB large page
pub const PAGES_PER_LARGE_PAGE: usize = 512;

// ═══════════════════════════════════════════════════════════════════════════
// Alignment Functions
// ═══════════════════════════════════════════════════════════════════════════

/// Align value up to alignment
#[inline]
pub const fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

/// Align value down to alignment
#[inline]
pub const fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

/// Check if value is aligned
#[inline]
pub const fn is_aligned(value: u64, align: u64) -> bool {
    value & (align - 1) == 0
}

/// Align to 4KB page boundary (up)
#[inline]
pub const fn page_align_up(value: u64) -> u64 {
    align_up(value, PAGE_SIZE)
}

/// Align to 4KB page boundary (down)
#[inline]
pub const fn page_align_down(value: u64) -> u64 {
    align_down(value, PAGE_SIZE)
}

/// Align to 2MB large page boundary (up)
#[inline]
pub const fn large_page_align_up(value: u64) -> u64 {
    align_up(value, LARGE_PAGE_SIZE)
}

/// Align to 2MB large page boundary (down)
#[inline]
pub const fn large_page_align_down(value: u64) -> u64 {
    align_down(value, LARGE_PAGE_SIZE)
}

/// Check if address is 2MB aligned
#[inline]
pub const fn is_large_page_aligned(value: u64) -> bool {
    is_aligned(value, LARGE_PAGE_SIZE)
}

/// Calculate number of 4KB pages needed for a given size
#[inline]
pub const fn pages_for_size(size: u64) -> u64 {
    size.div_ceil(PAGE_SIZE)
}

/// Calculate number of 2MB large pages needed for a given size
#[inline]
pub const fn large_pages_for_size(size: u64) -> u64 {
    size.div_ceil(LARGE_PAGE_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 4096), 0);
        assert_eq!(align_up(1, 4096), 4096);
        assert_eq!(align_up(4096, 4096), 4096);
        assert_eq!(align_up(4097, 4096), 8192);
    }

    #[test]
    fn test_align_down() {
        assert_eq!(align_down(0, 4096), 0);
        assert_eq!(align_down(1, 4096), 0);
        assert_eq!(align_down(4096, 4096), 4096);
        assert_eq!(align_down(4097, 4096), 4096);
    }

    #[test]
    fn test_is_aligned() {
        assert!(is_aligned(0, 4096));
        assert!(!is_aligned(1, 4096));
        assert!(is_aligned(4096, 4096));
        assert!(!is_aligned(4097, 4096));
    }

    #[test]
    fn test_page_align() {
        assert_eq!(page_align_up(0), 0);
        assert_eq!(page_align_up(1), 4096);
        assert_eq!(page_align_up(4096), 4096);
        assert_eq!(page_align_down(4097), 4096);
    }
}
