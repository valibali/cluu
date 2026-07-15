//! Address Space Management
//!
//! This module manages virtual address spaces for processes, preserving
//! the battle-tested memory layout from the reference implementation.
//!
//! # Memory Layout (Preserved from Reference)
//!
//! ```text
//! USERSPACE (Ring 3):
//! 0x00000000 - 0x00400000   Reserved (NULL pointer protection, 4MB)
//! 0x00400000 - 0x00600000   Text segment (code, read+execute, 2MB)
//! 0x00600000 - 0x00800000   Data/BSS segment (data, read+write, 2MB)
//! 0x00800000 - 0x40000000   Heap (grows up via _sbrk, lazy allocated, ~1GB)
//! 0x7ff00000 - 0x80000000   Stack (grows down, 16 MB)
//!
//! KERNEL (Ring 0):
//! 0xffff800000000000+       Physmap (direct map of all physical memory)
//! 0xffffffffffe00000        BOOTBOOT info structures
//! 0xffffffffffe02000        Kernel code/data
//! 0xfffffffffffc000000      BOOTBOOT framebuffer
//! 0xfffffffffffc000000      Kernel heap (8 MiB)
//! ```
//!
//! # Design Principles
//!
//! - Preserves the exact memory layout from the reference implementation
//! - Uses PageTableManager (Phase 3) for page table operations
//! - Uses any PageAllocator implementation (Phase 2) for frame allocation
//! - Implements AddressSpaceManager trait for clean architecture

use crate::mm::traits::PageFlags;
use alloc::boxed::Box;
use x86_64::{PhysAddr, VirtAddr};

/// Memory layout constants (preserved from reference)
pub mod layout {
    // NULL pointer protection - first 4MB reserved
    pub const USER_NULL_REGION_END: u64 = 0x0040_0000;

    // Text segment (code) - 2MB
    pub const USER_TEXT_START: u64 = 0x0040_0000;
    pub const USER_TEXT_SIZE: usize = 2 * 1024 * 1024;

    // Data/BSS segment - 2MB
    pub const USER_DATA_START: u64 = 0x0060_0000;
    pub const USER_DATA_SIZE: usize = 2 * 1024 * 1024;

    // Heap - starts at 8MB, can grow to 1GB
    pub const USER_HEAP_START: u64 = 0x0080_0000;
    pub const USER_HEAP_MAX: u64 = 0x4000_0000;

    // Stack - 16MB at top of user space (grows down)
    pub const USER_STACK_SIZE: usize = 16 * 1024 * 1024;
    pub const USER_STACK_TOP: u64 = 0x8000_0000;
    pub const USER_STACK_BOTTOM: u64 = USER_STACK_TOP - USER_STACK_SIZE as u64;

    /// Upper bound (exclusive) for `space_grant` source/target VAs.
    ///
    /// Higher than `USER_STACK_TOP` so VFS-style services that need large
    /// shared regions (cache, rings, bounce buffers) can place them above
    /// the user stack without losing the ability to space_grant them to
    /// peers. Must stay within PML4 entry 0 (≤ 512 GB) to remain in the
    /// user half. 4 GB is enough headroom for current and near-future
    /// per-service buffers without risking PML4 entry collisions.
    pub const USER_GRANT_TOP: u64 = 0x1_0000_0000;

    // Physmap base (direct map of physical memory)
    pub const PHYS_MAP_BASE: u64 = 0xffff_8000_0000_0000;

    // BOOTBOOT info structures
    pub const BOOTBOOT_INFO_BASE: u64 = 0xffff_ffff_ffe0_0000;

    // Kernel code/data
    pub const KERNEL_OFFSET: u64 = 0xffff_ffff_ffe0_2000;

    // Kernel heap
    pub const KERNEL_HEAP_START: u64 = 0xffff_ffff_c000_0000;
    pub const KERNEL_HEAP_SIZE: usize = 16 * 1024 * 1024; // 16 MiB (updated to match heap.rs)

    // Local APIC MMIO window
    pub const KERNEL_APIC_BASE: u64 = 0xffff_ffff_fec0_0000;
}

/// Memory region descriptor
///
/// Describes a contiguous region of virtual memory with specific flags.
/// This matches the reference implementation's MemoryRegion structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    /// Starting virtual address
    pub start: VirtAddr,
    /// Size in bytes
    pub size: usize,
    /// Page flags for this region
    pub flags: PageFlags,
}

/// Source data for demand-paged text segments (M9).
///
/// The source text bytes are copied into a kernel heap buffer at install
/// time (when `invoke_space_protect` is called with
/// `PROTECT_INSTALL_UNMAPPED`). At fault time the kernel allocates a fresh
/// frame and copies `source_data[page_offset..page_offset+bytes_to_copy]`
/// into it; bytes beyond `source_data.len()` are zero-filled (the frame is
/// zeroed before copy). Copying at install time avoids translating the
/// source space's page table at fault time, which could fail if the source
/// pages are evicted or retagged between install and fault.
#[derive(Debug)]
pub struct TextSource {
    /// Kernel heap buffer containing the source text bytes, copied at
    /// install time. Freed automatically when TextSource is dropped.
    pub source_data: Box<[u8]>,
    /// Original source vaddr (for diagnostics only).
    pub source_vaddr: u64,
}

impl MemoryRegion {
    /// Create a new memory region
    pub const fn new(start: VirtAddr, size: usize, flags: PageFlags) -> Self {
        Self { start, size, flags }
    }

    /// Get the ending address (exclusive)
    pub fn end(&self) -> VirtAddr {
        VirtAddr::new(self.start.as_u64() + self.size as u64)
    }

    /// Check if an address is within this region
    pub fn contains(&self, addr: VirtAddr) -> bool {
        addr >= self.start && addr < self.end()
    }
}

/// Heap region with lazy allocation
///
/// Represents a heap that grows on demand via page faults.
/// Matches the reference implementation's HeapRegion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapRegion {
    /// Base address of heap
    start: VirtAddr,
    /// Current break (end of allocated heap)
    current_brk: VirtAddr,
    /// Maximum heap address (exclusive)
    max: VirtAddr,
}

impl HeapRegion {
    /// Create a new heap region
    ///
    /// Initially, current_brk == start (heap is empty).
    pub const fn new(start: VirtAddr, max: VirtAddr) -> Self {
        Self {
            start,
            current_brk: start,
            max,
        }
    }

    /// Check if an address is within the allocated heap
    ///
    /// Only returns true for [start..current_brk).
    pub fn contains_allocated(&self, addr: VirtAddr) -> bool {
        addr >= self.start && addr < self.current_brk
    }

    /// Check if an address is within the valid heap range
    ///
    /// Returns true for [start..max), even if not yet allocated.
    /// Used by page fault handler to determine if a fault should trigger allocation.
    pub fn contains_valid(&self, addr: VirtAddr) -> bool {
        addr >= self.start && addr < self.max
    }

    /// Get current heap size in bytes
    pub fn size(&self) -> usize {
        (self.current_brk.as_u64() - self.start.as_u64()) as usize
    }

    /// Grow heap by increment bytes
    ///
    /// Returns new brk on success, None if would exceed max.
    /// Note: This does NOT allocate physical pages - that happens
    /// on first access via page fault handler.
    pub fn grow(&mut self, increment: isize) -> Option<VirtAddr> {
        let new_brk = if increment >= 0 {
            self.current_brk.as_u64().checked_add(increment as u64)?
        } else {
            self.current_brk.as_u64().checked_sub((-increment) as u64)?
        };

        let new_brk = VirtAddr::new(new_brk);

        // Validate new brk is within bounds
        if new_brk < self.start || new_brk > self.max {
            return None;
        }

        self.current_brk = new_brk;
        Some(new_brk)
    }

    /// Get the current break (end of allocated heap)
    pub fn current_brk(&self) -> VirtAddr {
        self.current_brk
    }

    /// Get the heap start address
    pub fn start(&self) -> VirtAddr {
        self.start
    }

    /// Get the heap maximum address
    pub fn max(&self) -> VirtAddr {
        self.max
    }
}

/// Address space for a process
///
/// Represents the complete virtual memory layout for a process,
/// including page table root and all memory regions.
///
/// This matches the reference implementation's AddressSpace structure
/// but uses our new PageTableManager for operations.
#[derive(Debug)]
pub struct AddressSpace {
    /// Physical address of PML4 (page table root)
    /// This is what goes into CR3 register during context switch
    pub page_table_root: PhysAddr,

    /// Text segment (code) - read+execute
    pub text: MemoryRegion,

    /// Data/BSS segment - read+write
    pub data: MemoryRegion,

    /// Heap region with lazy allocation
    pub heap: HeapRegion,

    /// Stack region - read+write, grows down
    pub stack: MemoryRegion,

    /// Per-process ASLR: first address above the stack guard page.
    ///
    /// The page-fault handler (idt.rs) looks this up by CR3 via
    /// `space_repository::with_space_by_pml4` and uses it instead of the
    /// global `layout::USER_STACK_BOTTOM + 0x1000` to correctly exclude the
    /// guard page from demand paging when ASLR randomizes the stack base.
    /// Initialized to the global default; updated by the kernel's
    /// `invoke_space_map_range` handler when a `MAP_GUARD` mapping is
    /// installed below the stack.
    pub aslr_stack_guard_end: u64,

    /// Source for demand-paged text (M9). None when text is eagerly mapped.
    pub text_source: Option<TextSource>,
}

impl AddressSpace {
    /// Create a new address space with the given page table root
    ///
    /// This is a low-level constructor. Use `build_kernel_space()` or
    /// `new_user()` for typical use cases.
    ///
    /// # Arguments
    ///
    /// * `page_table_root` - Physical address of PML4
    ///
    /// # Returns
    ///
    /// A new AddressSpace with empty user regions.
    pub fn new(page_table_root: PhysAddr) -> Self {
        let null_region = MemoryRegion::new(VirtAddr::new(0), 0, PageFlags::kernel());

        let heap = HeapRegion::new(
            VirtAddr::new(layout::USER_HEAP_START),
            VirtAddr::new(layout::USER_HEAP_MAX),
        );

        Self {
            page_table_root,
            text: null_region,
            data: null_region,
            heap,
            stack: null_region,
            aslr_stack_guard_end: layout::USER_STACK_BOTTOM + 0x1000,
            text_source: None,
        }
    }

    /// Get the kernel address space
    ///
    /// This creates an AddressSpace descriptor for the currently active
    /// kernel page tables. Unlike `build_kernel_space()`, this doesn't
    /// create new page tables - it just wraps the current CR3.
    ///
    /// Use this for kernel threads that share the main kernel address space.
    pub fn current_kernel() -> Self {
        use x86_64::registers::control::Cr3;

        let (current_pml4_frame, _) = Cr3::read();
        let page_table_root = current_pml4_frame.start_address();

        Self::new(page_table_root)
    }

    /// Create a new kernel address space (alias for current_kernel)
    ///
    /// This is for compatibility with old code that expects `new_kernel()`.
    /// Kernel threads share the same address space, so this returns the current one.
    pub fn new_kernel() -> Self {
        Self::current_kernel()
    }

    /// Create a new userspace address space
    ///
    /// This allocates a fresh PML4 and copies the kernel half from the
    /// current (kernel) page tables. User regions are initially empty
    /// and will be filled by the ELF loader.
    ///
    /// Steps:
    /// 1. Allocate new PML4
    /// 2. Copy kernel mappings (high half) from current page tables
    /// 3. Set up user region descriptors (initially unmapped)
    ///
    /// # Returns
    ///
    /// A new AddressSpace with its own page tables, or error if out of memory.
    pub fn new_user() -> Result<Self, &'static str> {
        use x86_64::registers::control::Cr3;

        // Allocate a new PML4 frame
        let pml4_phys = unsafe { super::vmm::alloc_pml4()? };

        klibcluu::trace("Allocated new user PML4 at 0x");
        klibcluu::log_hex(klibcluu::LogLevel::Trace, "", pml4_phys.as_u64());

        // Copy kernel mappings from current page table
        let (current_pml4_frame, _) = Cr3::read();
        let current_pml4_phys = current_pml4_frame.start_address();

        unsafe {
            super::vmm::copy_kernel_half(current_pml4_phys, pml4_phys);
        }

        klibcluu::trace("Copied kernel mappings to new user PML4");

        let mut space = Self::new(pml4_phys);
        // Record the canonical stack region so space_get_stats /
        // count_user_pages can classify heap pages (range
        // [heap_start, stack_start)) correctly.  Without this,
        // stack.start stays 0 and no page is ever counted as heap.
        space.set_stack(
            VirtAddr::new(layout::USER_STACK_BOTTOM),
            layout::USER_STACK_SIZE,
        );
        Ok(space)
    }

    /// Switch to this address space
    ///
    /// Updates CR3 register to point to this process's page table.
    /// This is called during context switches between processes.
    ///
    /// CRITICAL: This invalidates the TLB, costing ~100 cycles to refill.
    ///
    /// # Safety
    ///
    /// - The page table must be properly set up with kernel mappings
    /// - Current stack and instruction pointer must be mapped in new tables
    pub unsafe fn switch_to(&self) {
        use x86_64::registers::control::Cr3;
        use x86_64::structures::paging::PhysFrame;

        let frame = PhysFrame::containing_address(self.page_table_root);
        let flags = Cr3::read().1; // Preserve current CR3 flags

        unsafe {
            Cr3::write(frame, flags);
        }
    }

    /// Check if an address is within user-accessible regions
    ///
    /// Used for validating pointers passed from userspace in syscalls.
    pub fn is_user_accessible(&self, addr: VirtAddr) -> bool {
        self.text.contains(addr)
            || self.data.contains(addr)
            || self.heap.contains_valid(addr)
            || self.stack.contains(addr)
    }

    /// Check if an address is valid for heap growth
    ///
    /// Used by page fault handler to determine if a fault in heap
    /// region should trigger lazy allocation.
    pub fn is_valid_heap_address(&self, addr: VirtAddr) -> bool {
        self.heap.contains_valid(addr)
    }

    /// Set the text segment region
    ///
    /// Called by ELF loader after mapping executable code.
    pub fn set_text(&mut self, start: VirtAddr, size: usize) {
        self.text = MemoryRegion::new(
            start,
            size,
            PageFlags {
                present: true,
                writable: false,
                user: true,
                no_execute: false,
                write_through: false,
                cache_disabled: false,
                accessed: false,
                dirty: false,
                huge: false,
                global: false,
            },
        );
    }

    /// Set the text segment region with a demand-paging source (M9).
    ///
    /// Installs the text region boundary AND records where the kernel can
    /// find the original text bytes when a not-present text page faults.
    pub fn set_text_with_source(
        &mut self,
        start: VirtAddr,
        size: usize,
        source: TextSource,
    ) {
        self.set_text(start, size);
        self.text_source = Some(source);
    }

    /// Set the data segment region
    ///
    /// Called by ELF loader after mapping read-write data.
    pub fn set_data(&mut self, start: VirtAddr, size: usize) {
        self.data = MemoryRegion::new(
            start,
            size,
            PageFlags {
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
            },
        );
    }

    /// Set the stack region
    ///
    /// Called by ELF loader after setting up initial stack.
    pub fn set_stack(&mut self, start: VirtAddr, size: usize) {
        self.stack = MemoryRegion::new(
            start,
            size,
            PageFlags {
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
            },
        );
    }

    /// Update the per-process ASLR stack guard boundary.
    ///
    /// Called by `invoke_space_map_range` when a `MAP_GUARD` mapping is
    /// installed below the stack. `guard_end` is the first address above
    /// the guard page (= stack base). The page-fault handler uses this
    /// to exclude the guard page from demand paging.
    pub fn set_aslr_stack_guard_end(&mut self, guard_end: u64) {
        self.aslr_stack_guard_end = guard_end;
    }

    /// Get a reference to the heap region
    pub fn heap(&self) -> &HeapRegion {
        &self.heap
    }

    /// Get a mutable reference to the heap region
    pub fn heap_mut(&mut self) -> &mut HeapRegion {
        &mut self.heap
    }
}

// Note: AddressSpaceManager trait is for a manager that handles multiple
// address spaces. AddressSpace itself doesn't implement it - instead, we'll
// create a separate SpaceManager struct that implements AddressSpaceManager
// and manages multiple AddressSpace instances.

#[cfg(test)]
mod tests {
    use super::*;

    /// Test memory region creation and containment
    #[test]
    fn test_memory_region() {
        let region = MemoryRegion::new(
            VirtAddr::new(0x1000),
            0x2000, // 8KB
            PageFlags::kernel(),
        );

        assert_eq!(region.start.as_u64(), 0x1000);
        assert_eq!(region.size, 0x2000);
        assert_eq!(region.end().as_u64(), 0x3000);

        // Test containment
        assert!(!region.contains(VirtAddr::new(0x0)));
        assert!(!region.contains(VirtAddr::new(0xfff)));
        assert!(region.contains(VirtAddr::new(0x1000)));
        assert!(region.contains(VirtAddr::new(0x2000)));
        assert!(region.contains(VirtAddr::new(0x2fff)));
        assert!(!region.contains(VirtAddr::new(0x3000)));
    }

    /// Test heap region growth
    #[test]
    fn test_heap_region_grow() {
        let mut heap = HeapRegion::new(
            VirtAddr::new(layout::USER_HEAP_START),
            VirtAddr::new(layout::USER_HEAP_MAX),
        );

        // Initially empty
        assert_eq!(heap.size(), 0);
        assert_eq!(heap.current_brk().as_u64(), layout::USER_HEAP_START);

        // Grow by 4KB
        let new_brk = heap.grow(4096).expect("grow failed");
        assert_eq!(new_brk.as_u64(), layout::USER_HEAP_START + 4096);
        assert_eq!(heap.size(), 4096);

        // Grow by another 4KB
        let new_brk = heap.grow(4096).expect("grow failed");
        assert_eq!(new_brk.as_u64(), layout::USER_HEAP_START + 8192);
        assert_eq!(heap.size(), 8192);

        // Shrink by 4KB
        let new_brk = heap.grow(-4096).expect("grow failed");
        assert_eq!(new_brk.as_u64(), layout::USER_HEAP_START + 4096);
        assert_eq!(heap.size(), 4096);
    }

    /// Test heap region containment
    #[test]
    fn test_heap_region_contains() {
        let mut heap = HeapRegion::new(VirtAddr::new(0x10000), VirtAddr::new(0x20000));

        // Initially, nothing is allocated
        assert!(!heap.contains_allocated(VirtAddr::new(0x10000)));
        assert!(!heap.contains_allocated(VirtAddr::new(0x15000)));

        // But the range is valid
        assert!(heap.contains_valid(VirtAddr::new(0x10000)));
        assert!(heap.contains_valid(VirtAddr::new(0x15000)));
        assert!(heap.contains_valid(VirtAddr::new(0x1ffff)));
        assert!(!heap.contains_valid(VirtAddr::new(0x20000)));

        // Grow the heap
        heap.grow(0x5000).expect("grow failed");

        // Now [0x10000..0x15000) is allocated
        assert!(heap.contains_allocated(VirtAddr::new(0x10000)));
        assert!(heap.contains_allocated(VirtAddr::new(0x14fff)));
        assert!(!heap.contains_allocated(VirtAddr::new(0x15000)));
    }

    /// Test heap growth limits
    #[test]
    fn test_heap_growth_limits() {
        let mut heap = HeapRegion::new(
            VirtAddr::new(0x10000),
            VirtAddr::new(0x20000), // 64KB max
        );

        // Can grow to max
        assert!(heap.grow(0x10000).is_some()); // 64KB

        // Cannot exceed max
        assert!(heap.grow(1).is_none());

        // Can shrink below start
        assert!(heap.grow(-0x20000).is_none());
    }

    /// Test address space user accessibility
    #[test]
    fn test_address_space_user_accessible() {
        let mut space = AddressSpace::new(PhysAddr::new(0x1000));

        // Set up regions
        space.set_text(
            VirtAddr::new(layout::USER_TEXT_START),
            layout::USER_TEXT_SIZE,
        );
        space.set_data(
            VirtAddr::new(layout::USER_DATA_START),
            layout::USER_DATA_SIZE,
        );
        space.set_stack(
            VirtAddr::new(layout::USER_STACK_BOTTOM),
            layout::USER_STACK_SIZE,
        );

        // Grow heap
        space.heap_mut().grow(4096).expect("heap grow failed");

        // Check accessibility
        assert!(!space.is_user_accessible(VirtAddr::new(0x0))); // NULL region
        assert!(space.is_user_accessible(VirtAddr::new(layout::USER_TEXT_START)));
        assert!(space.is_user_accessible(VirtAddr::new(layout::USER_DATA_START)));
        assert!(space.is_user_accessible(VirtAddr::new(layout::USER_HEAP_START)));
        assert!(!space.is_user_accessible(VirtAddr::new(layout::USER_HEAP_START + 0x10000))); // Beyond heap brk
        assert!(space.is_user_accessible(VirtAddr::new(layout::USER_STACK_BOTTOM)));
    }

    /// Test memory layout constants match reference
    #[test]
    fn test_memory_layout_constants() {
        // These MUST match the reference implementation
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
}
