//! Memory Management Tests
//!
//! Tests for PMM, VMM, address spaces, and physmap.

#[test]
fn test_memory_layout_constants() {
    use kernel_tests::cluu_kernel::mm::layout;

    // Verify memory layout matches reference implementation
    assert_eq!(layout::USER_NULL_REGION_END, 0x0040_0000);
    assert_eq!(layout::USER_TEXT_START, 0x0040_0000);
    assert_eq!(layout::USER_TEXT_SIZE, 2 * 1024 * 1024);
    assert_eq!(layout::USER_DATA_START, 0x0060_0000);
    assert_eq!(layout::USER_DATA_SIZE, 2 * 1024 * 1024);
    assert_eq!(layout::USER_HEAP_START, 0x0080_0000);
    assert_eq!(layout::USER_HEAP_MAX, 0x4000_0000);
    assert_eq!(layout::USER_STACK_SIZE, 16 * 1024 * 1024);
    assert_eq!(layout::USER_STACK_TOP, 0x8000_0000);
    assert_eq!(layout::PHYS_MAP_BASE, 0xffff_8000_0000_0000);
}

// TODO: Add more memory management tests
// - HeapRegion::new() and grow()
// - MemoryRegion containment checks
// - Address space creation
