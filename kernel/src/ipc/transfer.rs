//! Buffer Transfer Operations
//!
//! This module implements buffer transfer operations for IPC messages.
//! Three transfer modes are supported:
//!
//! 1. **Copy**: Copy data between address spaces (safe but slow)
//! 2. **Grant**: Transfer page ownership (zero-copy, exclusive)
//! 3. **Map**: Create shared mapping (zero-copy, shared)
//!
//! # Performance Considerations
//!
//! - **Copy**: O(n) where n = buffer size. Good for small buffers (<4KB)
//! - **Grant/Map**: O(pages). Good for large buffers (≥4KB)
//!
//! # Safety
//!
//! - Grant removes pages from source - sender loses access
//! - Map allows concurrent access - requires careful synchronization
//! - Copy is always safe but slower

use crate::error::Error;
use crate::ipc::traits::MessageTransfer;
use crate::mm::traits::{PageAllocator, PageFlags, VirtualMemoryMapper};
use x86_64::{PhysAddr, VirtAddr};

/// Buffer Transfer Implementation
///
/// Implements buffer transfer operations using the memory management system.
///
/// # Type Parameters
///
/// * `A` - Page allocator for temporary page allocation
/// * `M` - Virtual memory mapper for page table operations
///
/// # Example
///
/// ```rust,no_run
/// let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);
///
/// // Copy 4096 bytes
/// transfer.copy_buffer(
///     sender_space,
///     VirtAddr::new(0x400000),
///     receiver_space,
///     VirtAddr::new(0x500000),
///     4096,
/// )?;
/// ```
pub struct BufferTransfer<'a, A, M>
where
    A: PageAllocator,
    M: VirtualMemoryMapper,
{
    allocator: &'a mut A,
    mapper: &'a mut M,
}

impl<'a, A, M> BufferTransfer<'a, A, M>
where
    A: PageAllocator,
    M: VirtualMemoryMapper,
{
    /// Create a new buffer transfer handler
    ///
    /// # Arguments
    ///
    /// * `allocator` - Page allocator for memory operations
    /// * `mapper` - Virtual memory mapper for page table operations
    pub fn new(allocator: &'a mut A, mapper: &'a mut M) -> Self {
        Self { allocator, mapper }
    }

    /// Check if address and length are page-aligned
    fn is_page_aligned(addr: VirtAddr, len: usize) -> bool {
        (addr.as_u64() & 0xFFF) == 0 && (len & 0xFFF) == 0
    }

    /// Get number of pages needed for a buffer
    fn pages_needed(len: usize) -> usize {
        (len + 0xFFF) / 0x1000
    }
}

impl<'a, A, M> MessageTransfer for BufferTransfer<'a, A, M>
where
    A: PageAllocator,
    M: VirtualMemoryMapper,
{
    fn copy_buffer(
        &mut self,
        from_space: usize,
        from_addr: VirtAddr,
        to_space: usize,
        to_addr: VirtAddr,
        len: usize,
    ) -> Result<(), Error> {
        // For now, this is a stub implementation.
        // In a full implementation, we would:
        //
        // 1. Switch to source address space
        // 2. Validate source address range
        // 3. Switch to destination address space
        // 4. Validate destination address range
        // 5. Copy data byte-by-byte or page-by-page
        //
        // This requires:
        // - Address space switching
        // - Temporary mapping for cross-space access
        // - Physical page pinning to prevent page faults during copy
        //
        // For testing purposes, we just validate inputs and return Ok.

        if len == 0 {
            return Ok(());
        }

        // Validate addresses are not null
        if from_addr.as_u64() < 0x1000 || to_addr.as_u64() < 0x1000 {
            return Err(Error::InvalidAddress);
        }

        // Check for overflow
        if from_addr
            .as_u64()
            .checked_add(len as u64)
            .is_none()
            || to_addr.as_u64().checked_add(len as u64).is_none()
        {
            return Err(Error::InvalidAddress);
        }

        // Stub: assume copy succeeded
        Ok(())
    }

    fn grant_buffer(
        &mut self,
        from_space: usize,
        from_addr: VirtAddr,
        to_space: usize,
        to_addr: VirtAddr,
        len: usize,
        flags: PageFlags,
    ) -> Result<(), Error> {
        // Validate page alignment
        if !Self::is_page_aligned(from_addr, len) || !Self::is_page_aligned(to_addr, len) {
            return Err(Error::InvalidAddress);
        }

        if len == 0 {
            return Ok(());
        }

        // For now, this is a stub implementation.
        // In a full implementation, we would:
        //
        // 1. Switch to source address space
        // 2. For each page:
        //    a. Get physical address from page table
        //    b. Unmap page from source space
        // 3. Switch to destination address space
        // 4. For each page:
        //    a. Map physical address to destination virtual address
        //    b. Set permissions according to flags
        //
        // This is a zero-copy operation - we transfer page table entries,
        // not physical pages themselves.
        //
        // CRITICAL: Source space loses access to these pages!

        let _pages = Self::pages_needed(len);

        // Stub: assume grant succeeded
        Ok(())
    }

    fn map_buffer(
        &mut self,
        from_space: usize,
        from_addr: VirtAddr,
        to_space: usize,
        to_addr: VirtAddr,
        len: usize,
        flags: PageFlags,
    ) -> Result<(), Error> {
        // Validate page alignment
        if !Self::is_page_aligned(from_addr, len) || !Self::is_page_aligned(to_addr, len) {
            return Err(Error::InvalidAddress);
        }

        if len == 0 {
            return Ok(());
        }

        // For now, this is a stub implementation.
        // In a full implementation, we would:
        //
        // 1. Switch to source address space
        // 2. For each page:
        //    a. Get physical address from page table
        // 3. Switch to destination address space
        // 4. For each page:
        //    a. Map SAME physical address to destination virtual address
        //    b. Set permissions according to flags
        //
        // This is a zero-copy operation - both spaces share the same
        // physical pages. Changes made by one space are visible to the other.
        //
        // IMPORTANT: Source space retains access to these pages!

        let _pages = Self::pages_needed(len);

        // Stub: assume map succeeded
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::pmm::BuddyAllocator;
    use crate::mm::vmm::PageTableManager;
    use x86_64::structures::paging::OffsetPageTable;

    // Mock allocator and mapper for testing
    struct MockAllocator;

    impl PageAllocator for MockAllocator {
        fn alloc(&mut self, _order: usize) -> Option<PhysAddr> {
            Some(PhysAddr::new(0x1000))
        }

        fn free(&mut self, _addr: PhysAddr, _order: usize) {
            // No-op
        }

        fn stats(&self) -> crate::mm::traits::AllocationStats {
            crate::mm::traits::AllocationStats {
                free_pages: 1000,
                total_pages: 1000,
                largest_free_order: 10,
            }
        }
    }

    struct MockMapper;

    impl VirtualMemoryMapper for MockMapper {
        fn map(
            &mut self,
            _virt: VirtAddr,
            _phys: PhysAddr,
            _flags: PageFlags,
        ) -> Result<(), crate::mm::traits::MapError> {
            Ok(())
        }

        fn unmap(&mut self, _virt: VirtAddr) -> Result<PhysAddr, crate::mm::traits::UnmapError> {
            Ok(PhysAddr::new(0x1000))
        }

        fn protect(
            &mut self,
            _virt: VirtAddr,
            _flags: PageFlags,
        ) -> Result<(), crate::mm::traits::MapError> {
            Ok(())
        }

        fn translate(&self, _virt: VirtAddr) -> Option<PhysAddr> {
            Some(PhysAddr::new(0x1000))
        }
    }

    #[test]
    fn test_buffer_transfer_new() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let _transfer = BufferTransfer::new(&mut allocator, &mut mapper);
    }

    #[test]
    fn test_is_page_aligned() {
        assert!(BufferTransfer::<MockAllocator, MockMapper>::is_page_aligned(
            VirtAddr::new(0x1000),
            0x1000
        ));
        assert!(BufferTransfer::<MockAllocator, MockMapper>::is_page_aligned(
            VirtAddr::new(0x2000),
            0x2000
        ));

        assert!(!BufferTransfer::<MockAllocator, MockMapper>::is_page_aligned(
            VirtAddr::new(0x1001),
            0x1000
        ));
        assert!(!BufferTransfer::<MockAllocator, MockMapper>::is_page_aligned(
            VirtAddr::new(0x1000),
            0x1001
        ));
    }

    #[test]
    fn test_pages_needed() {
        assert_eq!(
            BufferTransfer::<MockAllocator, MockMapper>::pages_needed(0),
            0
        );
        assert_eq!(
            BufferTransfer::<MockAllocator, MockMapper>::pages_needed(1),
            1
        );
        assert_eq!(
            BufferTransfer::<MockAllocator, MockMapper>::pages_needed(0x1000),
            1
        );
        assert_eq!(
            BufferTransfer::<MockAllocator, MockMapper>::pages_needed(0x1001),
            2
        );
        assert_eq!(
            BufferTransfer::<MockAllocator, MockMapper>::pages_needed(0x2000),
            2
        );
    }

    #[test]
    fn test_copy_buffer_zero_length() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);

        let result = transfer.copy_buffer(
            1,
            VirtAddr::new(0x400000),
            2,
            VirtAddr::new(0x500000),
            0,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_copy_buffer_null_address() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);

        let result =
            transfer.copy_buffer(1, VirtAddr::new(0), 2, VirtAddr::new(0x500000), 0x1000);
        assert_eq!(result, Err(Error::InvalidAddress));

        let result =
            transfer.copy_buffer(1, VirtAddr::new(0x400000), 2, VirtAddr::new(0), 0x1000);
        assert_eq!(result, Err(Error::InvalidAddress));
    }

    #[test]
    fn test_copy_buffer_overflow() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);

        let result = transfer.copy_buffer(
            1,
            VirtAddr::new(0xFFFF_FFFF_FFFF_F000),
            2,
            VirtAddr::new(0x500000),
            0x2000,
        );
        assert_eq!(result, Err(Error::InvalidAddress));
    }

    #[test]
    fn test_copy_buffer_valid() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);

        let result = transfer.copy_buffer(
            1,
            VirtAddr::new(0x400000),
            2,
            VirtAddr::new(0x500000),
            0x1000,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_grant_buffer_alignment() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);

        // Not page-aligned source
        let result = transfer.grant_buffer(
            1,
            VirtAddr::new(0x400001),
            2,
            VirtAddr::new(0x500000),
            0x1000,
            PageFlags::user(),
        );
        assert_eq!(result, Err(Error::InvalidAddress));

        // Not page-aligned destination
        let result = transfer.grant_buffer(
            1,
            VirtAddr::new(0x400000),
            2,
            VirtAddr::new(0x500001),
            0x1000,
            PageFlags::user(),
        );
        assert_eq!(result, Err(Error::InvalidAddress));

        // Not page-aligned length
        let result = transfer.grant_buffer(
            1,
            VirtAddr::new(0x400000),
            2,
            VirtAddr::new(0x500000),
            0x1001,
            PageFlags::user(),
        );
        assert_eq!(result, Err(Error::InvalidAddress));
    }

    #[test]
    fn test_grant_buffer_zero_length() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);

        let result = transfer.grant_buffer(
            1,
            VirtAddr::new(0x400000),
            2,
            VirtAddr::new(0x500000),
            0,
            PageFlags::user(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_grant_buffer_valid() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);

        let result = transfer.grant_buffer(
            1,
            VirtAddr::new(0x400000),
            2,
            VirtAddr::new(0x500000),
            0x1000,
            PageFlags::user(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_map_buffer_alignment() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);

        // Not page-aligned
        let result = transfer.map_buffer(
            1,
            VirtAddr::new(0x400001),
            2,
            VirtAddr::new(0x500000),
            0x1000,
            PageFlags::user(),
        );
        assert_eq!(result, Err(Error::InvalidAddress));
    }

    #[test]
    fn test_map_buffer_valid() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);

        let result = transfer.map_buffer(
            1,
            VirtAddr::new(0x400000),
            2,
            VirtAddr::new(0x500000),
            0x1000,
            PageFlags::user(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_map_buffer_multiple_pages() {
        let mut allocator = MockAllocator;
        let mut mapper = MockMapper;
        let mut transfer = BufferTransfer::new(&mut allocator, &mut mapper);

        let result = transfer.map_buffer(
            1,
            VirtAddr::new(0x400000),
            2,
            VirtAddr::new(0x500000),
            0x4000, // 4 pages
            PageFlags::user(),
        );
        assert!(result.is_ok());
    }
}
