//! Physical Memory Manager - Buddy Allocator
//!
//! This module implements a buddy allocator for managing physical memory frames.
//!
//! # Algorithm
//! The buddy allocator manages memory in power-of-2 sized blocks. Each allocation
//! request is rounded up to the nearest power of 2, and blocks can be split or
//! coalesced to satisfy requests efficiently.
//!
//! # Key Features
//! - O(log n) allocation and deallocation
//! - Automatic coalescing of adjacent free blocks
//! - Minimal fragmentation for power-of-2 sized allocations
//! - Support for multiple memory regions

#![cfg_attr(not(test), no_std)]

extern crate alloc;
use super::traits::{AllocationStats, PageAllocator};
use alloc::vec::Vec;
use x86_64::PhysAddr;

/// Page size constant (4 KiB)
const PAGE_SIZE: u64 = 0x1000;

/// Maximum order supported (2^MAX_ORDER pages)
/// Order 12 = 4096 pages = 16 MB max allocation
const MAX_ORDER: usize = 12;

/// Memory region descriptor
///
/// Represents a contiguous region of physical memory that can be managed
/// by the buddy allocator.
#[derive(Debug, Clone, Copy)]
pub struct MemoryRegion {
    /// Base physical address (must be page-aligned)
    pub base: u64,
    /// Size in bytes (will be rounded down to page boundary)
    pub size: u64,
}

impl MemoryRegion {
    /// Create a new memory region
    ///
    /// # Arguments
    /// * `base` - Base physical address (should be page-aligned)
    /// * `size` - Size in bytes
    pub fn new(base: u64, size: u64) -> Self {
        Self { base, size }
    }

    /// Get the number of pages in this region
    fn page_count(&self) -> usize {
        (self.size / PAGE_SIZE) as usize
    }

    /// Get the end address (exclusive)
    fn end(&self) -> u64 {
        self.base + self.size
    }
}

/// Free block in the buddy allocator
#[derive(Debug, Clone, Copy)]
struct FreeBlock {
    /// Physical address of the block
    addr: u64,
}

/// Buddy Allocator
///
/// Manages physical memory using the buddy allocation algorithm.
/// Maintains free lists for each order, allowing efficient allocation
/// and deallocation with automatic coalescing.
pub struct BuddyAllocator {
    /// Free lists for each order [0..=MAX_ORDER]
    /// free_lists[i] contains blocks of size 2^i pages
    free_lists: [Vec<FreeBlock>; MAX_ORDER + 1],

    /// Total number of pages managed
    total_pages: usize,

    /// Number of currently free pages
    free_pages: usize,

    /// Memory regions being managed
    regions: Vec<MemoryRegion>,
}

impl BuddyAllocator {
    /// Create a new buddy allocator from memory regions
    ///
    /// # Arguments
    /// * `regions` - Slice of memory regions to manage
    ///
    /// # Returns
    /// A new BuddyAllocator initialized with the given regions
    ///
    /// # Example
    /// ```
    /// let regions = [MemoryRegion::new(0x100000, 0x100000)]; // 1 MB at 1 MB
    /// let allocator = BuddyAllocator::new(&regions);
    /// ```
    pub fn new(regions: &[MemoryRegion]) -> Self {
        // Initialize free lists
        let free_lists: [Vec<FreeBlock>; MAX_ORDER + 1] = [
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ];

        let mut allocator = Self {
            free_lists,
            total_pages: 0,
            free_pages: 0,
            regions: Vec::new(),
        };

        // Add each region to the allocator
        for region in regions {
            allocator.add_region(*region);
        }

        allocator
    }

    /// Add a memory region to the allocator
    fn add_region(&mut self, region: MemoryRegion) {
        let page_count = region.page_count();
        if page_count == 0 {
            return;
        }

        self.total_pages += page_count;
        self.free_pages += page_count;
        self.regions.push(region);

        // Add the region as free blocks
        // We add the largest aligned blocks possible
        let mut addr = region.base;
        let end = region.end();

        while addr < end {
            let remaining_bytes = end - addr;
            let remaining_pages = (remaining_bytes / PAGE_SIZE) as usize;

            if remaining_pages == 0 {
                break;
            }

            // Find the largest order block that:
            // 1. Fits in the remaining space
            // 2. The address is properly aligned for that order
            //
            // A block of order N must be aligned to (2^N * PAGE_SIZE)
            let mut order = 0;
            for test_order in (0..=MAX_ORDER).rev() {
                let block_size_pages = 1usize << test_order;
                let block_size_bytes = (block_size_pages as u64) * PAGE_SIZE;

                // Check if this order fits in remaining space
                if block_size_pages > remaining_pages {
                    continue;
                }

                // Check if addr is aligned for this order
                if addr % block_size_bytes == 0 {
                    order = test_order;
                    break;
                }
            }

            // Add block to free list
            self.free_lists[order].push(FreeBlock { addr });

            // Move to next block
            let block_size = (1 << order) * PAGE_SIZE;
            addr += block_size;
        }
    }

    /// Allocate 2^order contiguous pages
    ///
    /// # Arguments
    /// * `order` - Allocation order where size = 2^order pages
    ///
    /// # Returns
    /// * `Some(PhysAddr)` - Physical address of allocated block
    /// * `None` - If allocation failed
    pub fn alloc(&mut self, order: usize) -> Option<PhysAddr> {
        if order > MAX_ORDER {
            return None;
        }

        // Try to find a free block of the requested order
        if let Some(block) = self.free_lists[order].pop() {
            // Found exact match
            let size_pages = 1 << order;
            self.free_pages = self.free_pages.saturating_sub(size_pages);
            return Some(PhysAddr::new(block.addr));
        }

        // No exact match, try to split a larger block
        for higher_order in (order + 1)..=MAX_ORDER {
            if let Some(block) = self.free_lists[higher_order].pop() {
                // Split this block down to the requested order
                return Some(self.split_block(block.addr, higher_order, order));
            }
        }

        // No suitable block found
        None
    }

    /// Split a block from higher_order down to target_order
    ///
    /// Returns the address of the allocated block.
    /// The buddy of each split is added to the appropriate free list.
    fn split_block(&mut self, addr: u64, higher_order: usize, target_order: usize) -> PhysAddr {
        let current_addr = addr;
        let mut current_order = higher_order;

        // Split down to target order
        while current_order > target_order {
            current_order -= 1;
            let block_size = (1 << current_order) * PAGE_SIZE;
            let buddy_addr = current_addr + block_size;

            // Add buddy to free list
            self.free_lists[current_order].push(FreeBlock { addr: buddy_addr });
        }

        // Update free page count
        let size_pages = 1 << target_order;
        self.free_pages = self.free_pages.saturating_sub(size_pages);

        PhysAddr::new(current_addr)
    }

    /// Free 2^order contiguous pages starting at addr
    ///
    /// # Arguments
    /// * `addr` - Physical address to free (must have been returned by alloc)
    /// * `order` - Order used in the original allocation
    pub fn free(&mut self, addr: PhysAddr, order: usize) {
        if order > MAX_ORDER {
            return;
        }

        let size_pages = 1 << order;
        self.free_pages += size_pages;

        // Try to coalesce with buddy
        self.free_and_coalesce(addr.as_u64(), order);
    }

    /// Free a block and coalesce with buddy if possible
    fn free_and_coalesce(&mut self, addr: u64, order: usize) {
        if order >= MAX_ORDER {
            // Cannot coalesce further, just add to free list
            self.free_lists[order].push(FreeBlock { addr });
            return;
        }

        // Calculate buddy address
        let block_size = (1 << order) * PAGE_SIZE;
        let buddy_addr = addr ^ block_size;

        // Check if buddy is free in the same order
        if let Some(buddy_idx) = self.find_free_block(order, buddy_addr) {
            // Buddy is free! Remove it and coalesce
            self.free_lists[order].swap_remove(buddy_idx);

            // The coalesced block starts at the lower address
            let coalesced_addr = core::cmp::min(addr, buddy_addr);

            // Recursively try to coalesce at higher order
            self.free_and_coalesce(coalesced_addr, order + 1);
        } else {
            // Buddy not free, just add this block
            self.free_lists[order].push(FreeBlock { addr });
        }
    }

    /// Find a free block at given order with given address
    ///
    /// Returns the index in the free list if found.
    fn find_free_block(&self, order: usize, addr: u64) -> Option<usize> {
        self.free_lists[order]
            .iter()
            .position(|block| block.addr == addr)
    }

    /// Get number of free pages
    pub fn free_pages(&self) -> usize {
        self.free_pages
    }

    /// Get total number of pages
    pub fn total_pages(&self) -> usize {
        self.total_pages
    }

    /// Calculate the largest free order available
    fn calculate_largest_free_order(&self) -> usize {
        for order in (0..=MAX_ORDER).rev() {
            if !self.free_lists[order].is_empty() {
                return order;
            }
        }
        0
    }
}

impl PageAllocator for BuddyAllocator {
    fn alloc(&mut self, order: usize) -> Option<PhysAddr> {
        Self::alloc(self, order)
    }

    fn free(&mut self, addr: PhysAddr, order: usize) {
        Self::free(self, addr, order)
    }

    fn stats(&self) -> AllocationStats {
        AllocationStats {
            free_pages: self.free_pages(),
            total_pages: self.total_pages(),
            largest_free_order: self.calculate_largest_free_order(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "proptest")]
    use proptest::prelude::*;

    // ─── Unit Tests ───

    #[test]
    fn test_new_empty_region() {
        let allocator = BuddyAllocator::new(&[]);
        assert_eq!(allocator.free_pages(), 0);
        assert_eq!(allocator.total_pages(), 0);
    }

    #[test]
    fn test_new_single_page_region() {
        let regions = [MemoryRegion::new(0x1000, 0x1000)];
        let allocator = BuddyAllocator::new(&regions);
        assert_eq!(allocator.free_pages(), 1);
        assert_eq!(allocator.total_pages(), 1);
    }

    #[test]
    fn test_alloc_single_page() {
        let regions = [MemoryRegion::new(0x1000, 0x1000)];
        let mut allocator = BuddyAllocator::new(&regions);

        let addr = allocator.alloc(0);
        assert_eq!(addr, Some(PhysAddr::new(0x1000)));
        assert_eq!(allocator.free_pages(), 0);
    }

    #[test]
    fn test_alloc_exhaustion() {
        let regions = [MemoryRegion::new(0x1000, 0x1000)];
        let mut allocator = BuddyAllocator::new(&regions);

        let _ = allocator.alloc(0);
        let second = allocator.alloc(0);
        assert_eq!(second, None);
    }

    #[test]
    fn test_free_and_realloc() {
        let regions = [MemoryRegion::new(0x1000, 0x1000)];
        let mut allocator = BuddyAllocator::new(&regions);

        let addr = allocator.alloc(0).unwrap();
        allocator.free(addr, 0);

        let addr2 = allocator.alloc(0);
        assert_eq!(addr2, Some(addr));
    }

    #[test]
    fn test_buddy_coalescing() {
        // Allocate 2 adjacent pages, free them, should coalesce to order 1
        // Region must be aligned to order 1 (0x2000) for buddy coalescing to work
        let regions = [MemoryRegion::new(0x2000, 0x2000)]; // Aligned to 8KB
        let mut allocator = BuddyAllocator::new(&regions);

        let a = allocator.alloc(0).unwrap();
        let b = allocator.alloc(0).unwrap();

        allocator.free(a, 0);
        allocator.free(b, 0);

        // Should be able to allocate order 1 (2 pages)
        let large = allocator.alloc(1);
        assert!(large.is_some());
    }

    #[test]
    fn test_splitting() {
        // Large region, allocate small, should split
        let regions = [MemoryRegion::new(0x1000, 0x10000)]; // 16 pages
        let mut allocator = BuddyAllocator::new(&regions);

        let _ = allocator.alloc(0); // 1 page
        assert_eq!(allocator.free_pages(), 15);
    }

    #[test]
    fn test_alignment_requirements() {
        // Order N allocations must be aligned to 2^N pages
        let regions = [MemoryRegion::new(0x1000, 0x100000)];
        let mut allocator = BuddyAllocator::new(&regions);

        for order in 0..8 {
            if let Some(addr) = allocator.alloc(order) {
                let alignment = 0x1000 << order;
                assert_eq!(
                    addr.as_u64() % alignment as u64,
                    0,
                    "Order {} allocation not properly aligned",
                    order
                );
                allocator.free(addr, order);
            }
        }
    }

    #[test]
    fn test_max_order_allocation() {
        // Region must be aligned to order 11 (0x800000 = 8MB) for max order allocation
        let regions = [MemoryRegion::new(0x800000, 0x800000)]; // 8MB aligned to 8MB
        let mut allocator = BuddyAllocator::new(&regions);

        // Should be able to allocate max order (8MB = 2048 pages = order 11)
        let big = allocator.alloc(11);
        assert!(big.is_some());
    }

    #[test]
    fn test_fragmentation_handling() {
        // Region must be aligned to order 2 (0x4000) for full coalescing
        let regions = [MemoryRegion::new(0x4000, 0x4000)]; // 4 pages aligned to 16KB
        let mut allocator = BuddyAllocator::new(&regions);

        // Allocate all 4 as single pages
        let a = allocator.alloc(0).unwrap();
        let b = allocator.alloc(0).unwrap();
        let c = allocator.alloc(0).unwrap();
        let d = allocator.alloc(0).unwrap();

        // Free alternating pages (fragmented)
        allocator.free(a, 0);
        allocator.free(c, 0);

        // Cannot allocate order 1 (2 contiguous pages)
        assert!(allocator.alloc(1).is_none());

        // Free remaining
        allocator.free(b, 0);
        allocator.free(d, 0);

        // Now should be able to allocate order 2 (4 pages coalesced)
        assert!(allocator.alloc(2).is_some());
    }

    #[test]
    fn test_multiple_regions() {
        let regions = [
            MemoryRegion::new(0x1000, 0x1000),   // 1 page
            MemoryRegion::new(0x10000, 0x10000), // 16 pages
        ];
        let allocator = BuddyAllocator::new(&regions);
        assert_eq!(allocator.total_pages(), 17);
        assert_eq!(allocator.free_pages(), 17);
    }

    #[test]
    fn test_stats() {
        // Region must be aligned to order 4 (0x10000 = 64KB) for proper block formation
        let regions = [MemoryRegion::new(0x10000, 0x10000)]; // 16 pages aligned to 64KB
        let mut allocator = BuddyAllocator::new(&regions);

        let stats = allocator.stats();
        assert_eq!(stats.total_pages, 16);
        assert_eq!(stats.free_pages, 16);
        assert!(stats.largest_free_order >= 4); // At least 16 pages

        // Allocate something
        let _ = allocator.alloc(2); // 4 pages
        let stats = allocator.stats();
        assert_eq!(stats.free_pages, 12);
    }

    // ─── Property-Based Tests ───

    #[cfg(feature = "proptest")]
    proptest! {
        #[test]
        fn prop_alloc_free_preserves_count(
            region_size in 1usize..=256,
            allocs in prop::collection::vec(0usize..8, 1..50)
        ) {
            let region_bytes = region_size * 0x1000;
            let regions = [MemoryRegion::new(0x10000, region_bytes as u64)];
            let mut allocator = BuddyAllocator::new(&regions);

            let initial = allocator.free_pages();
            let mut allocated = Vec::new();

            // Allocate
            for order in &allocs {
                if let Some(addr) = allocator.alloc(*order) {
                    allocated.push((addr, *order));
                }
            }

            // Free all
            for (addr, order) in allocated {
                allocator.free(addr, order);
            }

            // Should have same free count
            assert_eq!(allocator.free_pages(), initial);
        }

        #[test]
        fn prop_no_overlapping_allocations(
            region_size in 4usize..=64,
            allocs in prop::collection::vec(0usize..4, 1..20)
        ) {
            let region_bytes = region_size * 0x1000;
            let regions = [MemoryRegion::new(0x10000, region_bytes as u64)];
            let mut allocator = BuddyAllocator::new(&regions);

            let mut allocated: Vec<(PhysAddr, usize)> = Vec::new();

            for order in allocs {
                if let Some(addr) = allocator.alloc(order) {
                    let size = 0x1000 << order;

                    // Check no overlap with existing allocations
                    for (existing_addr, existing_order) in &allocated {
                        let existing_size = 0x1000 << existing_order;
                        let existing_end = existing_addr.as_u64() + existing_size as u64;
                        let new_end = addr.as_u64() + size as u64;

                        let overlaps = !(addr.as_u64() >= existing_end
                            || new_end <= existing_addr.as_u64());
                        assert!(!overlaps, "Overlapping allocations!");
                    }

                    allocated.push((addr, order));
                }
            }
        }
    }
}
