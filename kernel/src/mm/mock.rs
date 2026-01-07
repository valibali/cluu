//! Mock Implementations for Testing
//!
//! This module provides mock implementations of memory management traits
//! for use in testing components that depend on memory allocation.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
use super::traits::{AllocationStats, PageAllocator};
use alloc::vec::Vec;
use x86_64::PhysAddr;

/// Mock page allocator for testing
///
/// This allocator doesn't actually manage real memory - it just tracks
/// allocations and can be configured to simulate failures.
///
/// # Example
/// ```
/// let mut allocator = MockPageAllocator::new();
/// let addr = allocator.alloc(0).unwrap();
/// assert_eq!(allocator.allocation_count(), 1);
/// allocator.free(addr, 0);
/// assert_eq!(allocator.allocation_count(), 0);
/// ```
pub struct MockPageAllocator {
    /// List of currently allocated blocks (address, order)
    allocations: Vec<(PhysAddr, usize)>,
    /// Next address to allocate (increments with each allocation)
    next_addr: u64,
    /// If true, all allocations will fail
    should_fail: bool,
}

impl MockPageAllocator {
    /// Create a new mock allocator
    ///
    /// Starts with next_addr at 0x1000 and should_fail = false
    pub fn new() -> Self {
        Self {
            allocations: Vec::new(),
            next_addr: 0x1000,
            should_fail: false,
        }
    }

    /// Configure whether allocations should fail
    ///
    /// # Example
    /// ```
    /// let mut allocator = MockPageAllocator::new();
    /// allocator.set_should_fail(true);
    /// assert!(allocator.alloc(0).is_none());
    /// ```
    pub fn set_should_fail(&mut self, fail: bool) {
        self.should_fail = fail;
    }

    /// Get the number of currently allocated blocks
    pub fn allocation_count(&self) -> usize {
        self.allocations.len()
    }

    /// Reset the allocator to initial state
    pub fn reset(&mut self) {
        self.allocations.clear();
        self.next_addr = 0x1000;
        self.should_fail = false;
    }
}

impl Default for MockPageAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl PageAllocator for MockPageAllocator {
    fn alloc(&mut self, order: usize) -> Option<PhysAddr> {
        if self.should_fail {
            return None;
        }

        let addr = PhysAddr::new(self.next_addr);
        let size = 0x1000 << order;
        self.next_addr += size;
        self.allocations.push((addr, order));
        Some(addr)
    }

    fn free(&mut self, addr: PhysAddr, order: usize) {
        self.allocations
            .retain(|(a, o)| !(*a == addr && *o == order));
    }

    fn stats(&self) -> AllocationStats {
        AllocationStats {
            free_pages: usize::MAX,
            total_pages: usize::MAX,
            largest_free_order: 12,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_allocator_basic() {
        let mut allocator = MockPageAllocator::new();
        assert_eq!(allocator.allocation_count(), 0);

        let addr = allocator.alloc(0).unwrap();
        assert_eq!(allocator.allocation_count(), 1);

        allocator.free(addr, 0);
        assert_eq!(allocator.allocation_count(), 0);
    }

    #[test]
    fn test_mock_allocator_failure() {
        let mut allocator = MockPageAllocator::new();
        allocator.set_should_fail(true);

        assert!(allocator.alloc(0).is_none());
        assert_eq!(allocator.allocation_count(), 0);
    }

    #[test]
    fn test_mock_allocator_multiple_allocs() {
        let mut allocator = MockPageAllocator::new();

        let addr1 = allocator.alloc(0).unwrap();
        let addr2 = allocator.alloc(1).unwrap();
        let addr3 = allocator.alloc(2).unwrap();

        assert_eq!(allocator.allocation_count(), 3);

        // Addresses should be different and increasing
        assert!(addr1.as_u64() < addr2.as_u64());
        assert!(addr2.as_u64() < addr3.as_u64());

        // Free middle one
        allocator.free(addr2, 1);
        assert_eq!(allocator.allocation_count(), 2);

        // Free remaining
        allocator.free(addr1, 0);
        allocator.free(addr3, 2);
        assert_eq!(allocator.allocation_count(), 0);
    }

    #[test]
    fn test_mock_allocator_reset() {
        let mut allocator = MockPageAllocator::new();

        let _ = allocator.alloc(0);
        let _ = allocator.alloc(1);
        assert_eq!(allocator.allocation_count(), 2);

        allocator.reset();
        assert_eq!(allocator.allocation_count(), 0);
        assert_eq!(allocator.next_addr, 0x1000);
    }

    #[test]
    fn test_mock_allocator_stats() {
        let allocator = MockPageAllocator::new();
        let stats = allocator.stats();

        // Mock allocator reports unlimited memory
        assert_eq!(stats.free_pages, usize::MAX);
        assert_eq!(stats.total_pages, usize::MAX);
        assert_eq!(stats.largest_free_order, 12);
    }
}
