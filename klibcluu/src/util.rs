//! Common utility functions

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

/// Page size constant
pub const PAGE_SIZE: u64 = 4096;

/// Align to page boundary (up)
#[inline]
pub const fn page_align_up(value: u64) -> u64 {
    align_up(value, PAGE_SIZE)
}

/// Align to page boundary (down)
#[inline]
pub const fn page_align_down(value: u64) -> u64 {
    align_down(value, PAGE_SIZE)
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
