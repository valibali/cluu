//! Page Fault Handler
//!
//! This module implements page fault handling for the CLUU microkernel,
//! supporting:
//! - Lazy heap allocation (allocate-on-demand)
//! - Protection violation detection
//! - Page not present handling
//! - Invalid address detection
//!
//! # Page Fault Flow
//!
//! 1. CPU generates page fault → IDT handler calls our handler
//! 2. Handler examines fault address and error code
//! 3. Determines if fault is valid (e.g., lazy heap) or invalid (e.g., NULL deref)
//! 4. For valid faults: allocate page and map it
//! 5. For invalid faults: return error (will kill process)
//!
//! # Design Principles
//!
//! - Single Responsibility: Only handles page faults
//! - Dependency Inversion: Depends on PageAllocator and VirtualMemoryMapper traits
//! - Clean error handling with detailed error types

use crate::mm::traits::{
    MapError, PageAllocator, PageFaultError, PageFaultErrorCode, PageFlags, VirtualMemoryMapper,
};
use crate::mm::space::AddressSpace;
use x86_64::VirtAddr;

/// Page Fault Handler
///
/// Handles page faults by examining the faulting address and error code,
/// then taking appropriate action (allocate page, report error, etc.).
///
/// # Type Parameters
///
/// * `A` - Page allocator type (e.g., BuddyAllocator)
/// * `M` - Virtual memory mapper type (e.g., PageTableManager)
pub struct FaultHandler<'a, A, M>
where
    A: PageAllocator,
    M: VirtualMemoryMapper,
{
    /// Physical memory allocator
    allocator: &'a mut A,

    /// Virtual memory mapper (page table manager)
    mapper: &'a mut M,
}

impl<'a, A, M> FaultHandler<'a, A, M>
where
    A: PageAllocator,
    M: VirtualMemoryMapper,
{
    /// Create a new page fault handler
    ///
    /// # Arguments
    ///
    /// * `allocator` - Physical memory allocator
    /// * `mapper` - Virtual memory mapper (page table manager)
    pub fn new(allocator: &'a mut A, mapper: &'a mut M) -> Self {
        Self { allocator, mapper }
    }

    /// Handle a page fault
    ///
    /// This is the main entry point called by the page fault IDT handler.
    ///
    /// # Arguments
    ///
    /// * `space` - The address space that faulted
    /// * `addr` - Virtual address that caused the fault
    /// * `error_code` - Error code from CPU
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Fault handled successfully
    /// * `Err(PageFaultError)` - Fault cannot be handled (kill process)
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// let mut handler = FaultHandler::new(&mut allocator, &mut mapper);
    ///
    /// match handler.handle(&mut space, fault_addr, error_code) {
    ///     Ok(()) => {
    ///         // Fault handled, return to process
    ///     }
    ///     Err(e) => {
    ///         // Kill the process
    ///         klibcluu::error!("Page fault error: {:?}", e);
    ///     }
    /// }
    /// ```
    pub fn handle(
        &mut self,
        space: &mut AddressSpace,
        addr: VirtAddr,
        error_code: PageFaultErrorCode,
    ) -> Result<(), PageFaultError> {
        // Check for NULL pointer dereference
        if addr.as_u64() < 0x1000 {
            return Err(PageFaultError::InvalidAccess);
        }

        // Check if this is a lazy heap allocation
        if space.is_valid_heap_address(addr) && !error_code.protection_violation {
            return self.handle_lazy_heap(addr);
        }

        // Check for protection violation
        if error_code.protection_violation {
            return Err(PageFaultError::InvalidAccess);
        }

        // Page not present in a mapped region
        if space.is_user_accessible(addr) {
            // This shouldn't happen - user regions should be fully mapped
            // or in the lazy heap range
            return Err(PageFaultError::Unrecoverable);
        }

        // Invalid address (not in any valid region)
        Err(PageFaultError::InvalidAccess)
    }

    /// Handle lazy heap allocation
    ///
    /// Allocates and maps a page for heap expansion.
    ///
    /// # Arguments
    ///
    /// * `addr` - Faulting address in heap region
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Page allocated and mapped successfully
    /// * `Err(PageFaultError)` - Out of memory or mapping failed
    fn handle_lazy_heap(&mut self, addr: VirtAddr) -> Result<(), PageFaultError> {
        // Allocate a physical page
        let phys_addr = self
            .allocator
            .alloc(0)
            .ok_or(PageFaultError::OutOfMemory)?;

        // Map it with user heap flags
        let flags = PageFlags {
            present: true,
            writable: true,
            user: true,
            no_execute: true,
            write_through: false,
            cache_disabled: false,
            accessed: false,
            dirty: false,
            huge: false,
            global: false,
        };

        self.mapper
            .map(addr, phys_addr, flags)
            .map_err(|e| match e {
                MapError::OutOfMemory => PageFaultError::OutOfMemory,
                MapError::AlreadyMapped => PageFaultError::InvalidAccess,
                MapError::InvalidAddress => PageFaultError::InvalidAccess,
            })?;

        Ok(())
    }
}

impl<'a, A, M> crate::mm::traits::PageFaultHandler for FaultHandler<'a, A, M>
where
    A: PageAllocator,
    M: VirtualMemoryMapper,
{
    fn handle_fault(
        &mut self,
        addr: VirtAddr,
        error_code: PageFaultErrorCode,
    ) -> Result<(), PageFaultError> {
        // This trait method doesn't have access to AddressSpace.
        // In practice, the IDT handler would call our handle() method directly.
        // For now, we treat any fault without space context as invalid.
        Err(PageFaultError::InvalidAccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::{BuddyAllocator, HeapRegion, MemoryRegion, MockPageAllocator, PageTableManager};
    use alloc::vec::Vec;

    /// Mock mapper for testing
    struct MockMapper {
        mapped_pages: Vec<VirtAddr>,
        should_fail: bool,
    }

    impl MockMapper {
        fn new() -> Self {
            Self {
                mapped_pages: Vec::new(),
                should_fail: false,
            }
        }

        fn set_fail(&mut self, fail: bool) {
            self.should_fail = fail;
        }

        fn was_mapped(&self, addr: VirtAddr) -> bool {
            let page_addr = VirtAddr::new(addr.as_u64() & !0xfff);
            self.mapped_pages.contains(&page_addr)
        }
    }

    impl VirtualMemoryMapper for MockMapper {
        fn map(
            &mut self,
            virt: VirtAddr,
            _phys: x86_64::PhysAddr,
            _flags: PageFlags,
        ) -> Result<(), MapError> {
            if self.should_fail {
                return Err(MapError::OutOfMemory);
            }
            let page_addr = VirtAddr::new(virt.as_u64() & !0xfff);
            self.mapped_pages.push(page_addr);
            Ok(())
        }

        fn unmap(&mut self, _virt: VirtAddr) -> Result<x86_64::PhysAddr, crate::mm::traits::UnmapError> {
            unimplemented!("MockMapper::unmap not needed for tests")
        }

        fn protect(&mut self, _virt: VirtAddr, _flags: PageFlags) -> Result<(), MapError> {
            unimplemented!("MockMapper::protect not needed for tests")
        }

        fn translate(&self, _virt: VirtAddr) -> Option<x86_64::PhysAddr> {
            unimplemented!("MockMapper::translate not needed for tests")
        }
    }

    /// Test NULL pointer dereference detection
    #[test]
    fn test_null_pointer_fault() {
        let mut allocator = MockPageAllocator::new();
        let mut mapper = MockMapper::new();
        let mut handler = FaultHandler::new(&mut allocator, &mut mapper);

        let mut space = AddressSpace::new(x86_64::PhysAddr::new(0x1000));

        let error_code = PageFaultErrorCode {
            protection_violation: false,
            write: false,
            user: true,
            reserved_write: false,
            instruction_fetch: false,
        };

        // NULL dereference should fail
        let result = handler.handle(&mut space, VirtAddr::new(0x0), error_code);
        assert!(matches!(result, Err(PageFaultError::InvalidAccess)));

        // Address < 0x1000 should also fail
        let result = handler.handle(&mut space, VirtAddr::new(0x500), error_code);
        assert!(matches!(result, Err(PageFaultError::InvalidAccess)));
    }

    /// Test lazy heap allocation
    #[test]
    fn test_lazy_heap_allocation() {
        let mut allocator = MockPageAllocator::new();
        let mut mapper = MockMapper::new();
        let mut handler = FaultHandler::new(&mut allocator, &mut mapper);

        let mut space = AddressSpace::new(x86_64::PhysAddr::new(0x1000));

        // Grow heap to make the address valid
        let heap_addr = VirtAddr::new(0x00800000); // USER_HEAP_START
        space.heap_mut().grow(0x1000).expect("heap grow failed");

        let error_code = PageFaultErrorCode {
            protection_violation: false, // Page not present
            write: true,
            user: true,
            reserved_write: false,
            instruction_fetch: false,
        };

        // Fault in heap region should succeed
        let result = handler.handle(&mut space, heap_addr, error_code);
        assert!(result.is_ok());

        // Page should be mapped
        assert!(mapper.was_mapped(heap_addr));
    }

    /// Test protection violation
    #[test]
    fn test_protection_violation() {
        let mut allocator = MockPageAllocator::new();
        let mut mapper = MockMapper::new();
        let mut handler = FaultHandler::new(&mut allocator, &mut mapper);

        let mut space = AddressSpace::new(x86_64::PhysAddr::new(0x1000));

        let error_code = PageFaultErrorCode {
            protection_violation: true, // Page IS present - this is a protection violation
            write: true,
            user: true,
            reserved_write: false,
            instruction_fetch: false,
        };

        let result = handler.handle(&mut space, VirtAddr::new(0x400000), error_code);
        assert!(matches!(result, Err(PageFaultError::InvalidAccess)));
    }

    /// Test invalid address fault
    #[test]
    fn test_invalid_address_fault() {
        let mut allocator = MockPageAllocator::new();
        let mut mapper = MockMapper::new();
        let mut handler = FaultHandler::new(&mut allocator, &mut mapper);

        let mut space = AddressSpace::new(x86_64::PhysAddr::new(0x1000));

        let error_code = PageFaultErrorCode {
            protection_violation: false,
            write: true,
            user: true,
            reserved_write: false,
            instruction_fetch: false,
        };

        // Address not in any valid region
        let result = handler.handle(&mut space, VirtAddr::new(0x90000000), error_code);
        assert!(matches!(result, Err(PageFaultError::InvalidAccess)));
    }

    /// Test out of memory during lazy allocation
    #[test]
    fn test_oom_during_lazy_allocation() {
        let mut allocator = MockPageAllocator::new();
        allocator.set_should_fail(true); // Make allocator fail

        let mut mapper = MockMapper::new();
        let mut handler = FaultHandler::new(&mut allocator, &mut mapper);

        let mut space = AddressSpace::new(x86_64::PhysAddr::new(0x1000));

        // Grow heap to make the address valid
        let heap_addr = VirtAddr::new(0x00800000);
        space.heap_mut().grow(0x1000).expect("heap grow failed");

        let error_code = PageFaultErrorCode {
            protection_violation: false,
            write: true,
            user: true,
            reserved_write: false,
            instruction_fetch: false,
        };

        // Should fail with OOM
        let result = handler.handle(&mut space, heap_addr, error_code);
        assert!(matches!(result, Err(PageFaultError::OutOfMemory)));
    }

    /// Test mapping failure during lazy allocation
    #[test]
    fn test_map_failure_during_lazy_allocation() {
        let mut allocator = MockPageAllocator::new();

        let mut mapper = MockMapper::new();
        mapper.set_fail(true); // Make mapper fail

        let mut handler = FaultHandler::new(&mut allocator, &mut mapper);

        let mut space = AddressSpace::new(x86_64::PhysAddr::new(0x1000));

        // Grow heap to make the address valid
        let heap_addr = VirtAddr::new(0x00800000);
        space.heap_mut().grow(0x1000).expect("heap grow failed");

        let error_code = PageFaultErrorCode {
            protection_violation: false,
            write: true,
            user: true,
            reserved_write: false,
            instruction_fetch: false,
        };

        // Should fail with mapping error
        let result = handler.handle(&mut space, heap_addr, error_code);
        assert!(matches!(result, Err(PageFaultError::OutOfMemory)));
    }
}
