//! Virtual address space allocator for userspace processes.
//!
//! Provides a simple allocator for managing virtual address regions,
//! primarily used for memory grants and mappings. Follows the seL4
//! philosophy of userspace-managed address space.
//!
//! # Usage
//!
//! ```
//! use libcluu::vspace::{VSpaceAllocator, VSPACE};
//!
//! // Allocate a region for a grant
//! let addr = VSPACE.lock().alloc(size)?;
//!
//! // Use the address for space_grant or mapping
//! // ...
//!
//! // Free when done
//! VSPACE.lock().free(addr, size);
//! ```

use alloc::collections::BTreeMap;
use spin::Mutex;
use crate::{Error, Result};

/// Page size constant (4KB)
pub const PAGE_SIZE: usize = 4096;

/// Default region for grant allocations (256MB starting at 0x50000000)
const GRANT_REGION_BASE: usize = 0x5000_0000;
const GRANT_REGION_SIZE: usize = 256 * 1024 * 1024;

/// Global vspace allocator instance
pub static VSPACE: Mutex<VSpaceAllocator> = Mutex::new(VSpaceAllocator::new());

/// Virtual address space allocator.
///
/// Manages a pool of virtual addresses for grants, mappings, etc.
/// Uses a simple first-fit allocation strategy with coalescing on free.
pub struct VSpaceAllocator {
    /// Base address of managed region
    base: usize,
    /// Size of managed region
    size: usize,
    /// Free regions: start_addr -> size
    free_regions: BTreeMap<usize, usize>,
    /// Allocated regions: start_addr -> size (for validation on free)
    allocated: BTreeMap<usize, usize>,
    /// Whether the allocator has been initialized
    initialized: bool,
}

impl VSpaceAllocator {
    /// Create a new uninitialized allocator.
    pub const fn new() -> Self {
        Self {
            base: GRANT_REGION_BASE,
            size: GRANT_REGION_SIZE,
            free_regions: BTreeMap::new(),
            allocated: BTreeMap::new(),
            initialized: false,
        }
    }

    /// Initialize with default grant region.
    fn init_default(&mut self) {
        if self.initialized {
            return;
        }
        self.free_regions.insert(self.base, self.size);
        self.initialized = true;
    }

    /// Initialize with a custom region.
    pub fn init_region(&mut self, base: usize, size: usize) {
        self.base = base;
        self.size = size;
        self.free_regions.clear();
        self.allocated.clear();
        self.free_regions.insert(base, size);
        self.initialized = true;
    }

    /// Allocate a region of the given size (page-aligned).
    ///
    /// Returns the base address of the allocated region.
    pub fn alloc(&mut self, size: usize) -> Result<usize> {
        self.init_default();

        // Round up to page size
        let size = (size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        if size == 0 {
            return Err(Error::InvalidArgument);
        }

        // First-fit search
        let mut found: Option<(usize, usize)> = None;
        for (&addr, &region_size) in &self.free_regions {
            if region_size >= size {
                found = Some((addr, region_size));
                break;
            }
        }

        let (addr, region_size) = found.ok_or(Error::OutOfMemory)?;

        // Remove the free region
        self.free_regions.remove(&addr);

        // If there's leftover space, add it back as a free region
        if region_size > size {
            self.free_regions.insert(addr + size, region_size - size);
        }

        // Track the allocation
        self.allocated.insert(addr, size);

        Ok(addr)
    }

    /// Allocate a region at a specific address (if available).
    ///
    /// Returns Ok(()) if successful, Err if the region is not available.
    pub fn alloc_at(&mut self, addr: usize, size: usize) -> Result<()> {
        self.init_default();

        // Round up to page size
        let size = (size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        if size == 0 || addr & (PAGE_SIZE - 1) != 0 {
            return Err(Error::InvalidArgument);
        }

        // Find a free region that contains this range
        let mut found: Option<(usize, usize)> = None;
        for (&region_addr, &region_size) in &self.free_regions {
            if region_addr <= addr && addr + size <= region_addr + region_size {
                found = Some((region_addr, region_size));
                break;
            }
        }

        let (region_addr, region_size) = found.ok_or(Error::InvalidArgument)?;

        // Remove the free region
        self.free_regions.remove(&region_addr);

        // Add back any space before the allocation
        if addr > region_addr {
            self.free_regions.insert(region_addr, addr - region_addr);
        }

        // Add back any space after the allocation
        let end = addr + size;
        let region_end = region_addr + region_size;
        if end < region_end {
            self.free_regions.insert(end, region_end - end);
        }

        // Track the allocation
        self.allocated.insert(addr, size);

        Ok(())
    }

    /// Free a previously allocated region.
    ///
    /// The address and size must match a previous allocation.
    pub fn free(&mut self, addr: usize, size: usize) -> Result<()> {
        // Round up to page size
        let size = (size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        // Verify this was actually allocated
        match self.allocated.remove(&addr) {
            Some(allocated_size) if allocated_size == size => {}
            Some(_) => {
                // Size mismatch - put it back and return error
                return Err(Error::InvalidArgument);
            }
            None => return Err(Error::InvalidArgument),
        }

        // Add the region back to free list
        self.free_regions.insert(addr, size);

        // Coalesce with adjacent regions
        self.coalesce(addr);

        Ok(())
    }

    /// Coalesce the region at `addr` with adjacent free regions.
    fn coalesce(&mut self, addr: usize) {
        let size = match self.free_regions.get(&addr) {
            Some(&s) => s,
            None => return,
        };

        // Check for region immediately after
        let end = addr + size;
        if let Some(&next_size) = self.free_regions.get(&end) {
            self.free_regions.remove(&end);
            *self.free_regions.get_mut(&addr).unwrap() += next_size;
        }

        // Check for region immediately before
        let mut prev_addr = None;
        for (&a, &s) in &self.free_regions {
            if a + s == addr {
                prev_addr = Some((a, s));
                break;
            }
        }
        if let Some((prev, _prev_size)) = prev_addr {
            let current_size = self.free_regions.remove(&addr).unwrap();
            *self.free_regions.get_mut(&prev).unwrap() += current_size;
        }
    }

    /// Get the total amount of free space.
    pub fn free_space(&self) -> usize {
        self.free_regions.values().sum()
    }

    /// Get the number of free regions (fragmentation indicator).
    pub fn free_region_count(&self) -> usize {
        self.free_regions.len()
    }
}

impl Default for VSpaceAllocator {
    fn default() -> Self {
        Self::new()
    }
}
