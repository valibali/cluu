/*
 * Address Space Management
 *
 * This module implements per-process address spaces for userspace support.
 * Each process has its own page table (PML4 root) providing memory isolation.
 *
 * Memory Layout:
 * ==============
 *
 * USERSPACE (Ring 3):
 * 0x00000000 - 0x00400000   Reserved (NULL pointer protection)
 * 0x00400000 - 0x00600000   Text segment (code, read+execute)
 * 0x00600000 - 0x00800000   Data/BSS segment (data, read+write)
 * 0x00800000 - 0x40000000   Heap (grows up via _sbrk, lazy allocated)
 * 0x7ff00000 - 0x80000000   Stack (grows down, 16 MB)
 *
 * KERNEL (Ring 0):
 * 0xffff800000000000+       Kernel code/data (higher half)
 * 0xffffffff_c0000000       Kernel heap (8 MiB, shared across all processes)
 *
 * Key Concepts:
 * =============
 *
 * 1. ISOLATION: Each process has separate page tables (CR3 register)
 * 2. KERNEL MAPPING: Kernel pages are mapped in all address spaces
 * 3. USER ACCESSIBLE: User pages have USER_ACCESSIBLE flag set
 * 4. LAZY ALLOCATION: Heap pages allocated on first access (page fault)
 * 5. COW (Future): Copy-on-write for fork() support
 *
 * Why this is important:
 * - Prevents processes from accessing each other's memory
 * - Enables proper userspace isolation
 * - Foundation for fork/exec
 * - Security: NULL pointer protection, non-executable heap
 */

use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::PageTableFlags,
};

/// Memory region descriptor
///
/// Describes a contiguous region of virtual memory with permissions.
#[derive(Debug, Clone, Copy)]
pub struct MemoryRegion {
    /// Start virtual address (inclusive)
    pub start: VirtAddr,
    /// Size in bytes
    pub size: usize,
    /// Page table flags for this region
    pub flags: PageTableFlags,
}

impl MemoryRegion {
    /// Create a new memory region
    pub fn new(start: VirtAddr, size: usize, flags: PageTableFlags) -> Self {
        Self { start, size, flags }
    }

    /// Get the end address (exclusive)
    pub fn end(&self) -> VirtAddr {
        self.start + self.size as u64
    }

    /// Check if an address is within this region
    pub fn contains(&self, addr: VirtAddr) -> bool {
        addr >= self.start && addr < self.end()
    }
}

/// Heap region with lazy allocation support
///
/// The heap grows upward from start. The current_brk marks the
/// boundary of allocated virtual memory. Physical pages are only
/// allocated when first accessed (via page faults).
#[derive(Debug, Clone, Copy)]
pub struct HeapRegion {
    /// Start of heap region (fixed)
    pub start: VirtAddr,
    /// Current break point (can grow via _sbrk)
    pub current_brk: VirtAddr,
    /// Maximum heap address (cannot grow beyond this)
    pub max: VirtAddr,
}

impl HeapRegion {
    /// Create a new heap region
    pub fn new(start: VirtAddr, max: VirtAddr) -> Self {
        Self {
            start,
            current_brk: start,
            max,
        }
    }

    /// Check if an address is in the allocated heap region (below brk)
    pub fn contains_allocated(&self, addr: VirtAddr) -> bool {
        addr >= self.start && addr < self.current_brk
    }

    /// Check if an address is in the valid heap range (below max)
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
}

/// Address space for a process
///
/// Represents the complete virtual memory layout for a process,
/// including page table root and all memory regions.
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
}

impl AddressSpace {
    /// Create a new kernel address space
    ///
    /// Kernel processes use the existing kernel page tables.
    /// All kernel pages are identity-mapped in high half.
    ///
    /// For now, this returns a placeholder - kernel threads share
    /// the same page tables as the kernel itself.
    ///
    /// Note: Even though kernel threads don't normally need a userspace heap,
    /// we provide one to enable testing of sys_brk from kernel mode.
    pub fn new_kernel() -> Self {
        // For kernel processes, we use the current page table
        // Read the actual CR3 value so sys_spawn can switch to it
        use x86_64::registers::control::Cr3;
        let (current_pml4_frame, _) = Cr3::read();
        let page_table_root = current_pml4_frame.start_address();

        // Kernel doesn't have user segments
        let null_region = MemoryRegion::new(
            VirtAddr::new(0),
            0,
            PageTableFlags::empty(),
        );

        // Give kernel process a test heap (for sys_brk testing from kernel mode)
        // This uses userspace address range even though it's a kernel process
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
        }
    }

    /// Create a new userspace address space
    ///
    /// This allocates a fresh PML4 page table and sets up the
    /// standard memory layout for a user process.
    ///
    /// Steps:
    /// 1. Allocate new PML4 (4KB page)
    /// 2. Copy kernel mappings (high half) from current page table
    /// 3. Set up user regions (initially unmapped, filled by ELF loader)
    pub fn new_user() -> Result<Self, &'static str> {
        use crate::memory::phys;

        // Allocate a new PML4 frame
        let pml4_frame = phys::alloc_frame()
            .ok_or("Failed to allocate PML4 frame")?;

        let pml4_phys = PhysAddr::new(pml4_frame.start_address());

        log::debug!("Allocated new PML4 at physical address: {:#x}", pml4_phys.as_u64());

        // Zero out the new PML4
        unsafe {
            let pml4_ptr = pml4_phys.as_u64() as *mut u64;
            for i in 0..512 {
                core::ptr::write_volatile(pml4_ptr.add(i), 0);
            }
        }

        // Copy kernel mappings from current page table
        // We copy entries 1-511 (skip entry 0 to avoid huge page conflicts).
        //
        // Entry 0 maps the lower half and contains BOOTBOOT's huge pages which
        // conflict with userspace addresses (0x400000). We skip it so userspace
        // can use the lower half freely.
        //
        // Entries 1-255 contain kernel code/data that MUST be accessible for SYSCALL.
        // Entries 256-511 are the high half kernel mappings.
        //
        // CRITICAL: SYSCALL doesn't switch CR3! When SYSCALL executes, we're still
        // using userspace page tables. The syscall_entry code needs to access kernel
        // data (like SYSCALL_SCRATCH), so kernel mappings MUST be present.
        //
        // NOTE: Kernel must be in PID 0 (kernel process) page tables when accessing
        // initrd, since userspace page tables don't have the initrd mapped.
        unsafe {
            use x86_64::registers::control::Cr3;
            let (current_pml4_frame, _) = Cr3::read();
            let current_pml4_phys = current_pml4_frame.start_address();
            let current_pml4_ptr = current_pml4_phys.as_u64() as *const u64;
            let new_pml4_ptr = pml4_phys.as_u64() as *mut u64;

            // Copy kernel entries (1-511), skip entry 0 to avoid huge page conflicts
            for i in 1..512 {
                let entry = core::ptr::read_volatile(current_pml4_ptr.add(i));
                core::ptr::write_volatile(new_pml4_ptr.add(i), entry);
            }
        }

        log::debug!("Copied kernel mappings to new PML4");

        // Initialize regions (will be filled by ELF loader)
        let null_region = MemoryRegion::new(
            VirtAddr::new(0),
            0,
            PageTableFlags::empty(),
        );

        // Set up heap region (initially empty, grows with sys_brk)
        let heap = HeapRegion::new(
            VirtAddr::new(layout::USER_HEAP_START),
            VirtAddr::new(layout::USER_HEAP_MAX),
        );

        Ok(Self {
            page_table_root: pml4_phys,
            text: null_region,
            data: null_region,
            heap,
            stack: null_region,
        })
    }

    /// Switch to this address space
    ///
    /// Updates CR3 register to point to this process's page table.
    /// This is called during context switches between processes.
    ///
    /// CRITICAL: This invalidates the TLB, costing ~100 cycles to refill.
    ///
    /// Note: The scheduler only calls this when switching to a DIFFERENT process,
    /// so we don't need to check if we're already using this page table.
    pub fn switch_to(&self) {
        // Switch to this process's page table by updating CR3
        unsafe {
            use x86_64::registers::control::Cr3;
            use x86_64::structures::paging::PhysFrame;

            let (old_frame, _) = Cr3::read();
            let old_cr3 = old_frame.start_address().as_u64();
            let new_cr3 = self.page_table_root.as_u64();

            if old_cr3 != new_cr3 {
                log::info!("CR3 switch: 0x{:x} → 0x{:x}", old_cr3, new_cr3);
            }

            let frame = PhysFrame::containing_address(self.page_table_root);
            Cr3::write(frame, x86_64::registers::control::Cr3Flags::empty());
        }
    }

    /// Check if an address is within user-accessible regions
    ///
    /// Used for validating pointers passed from userspace in syscalls.
    pub fn is_user_accessible(&self, addr: VirtAddr) -> bool {
        // Check if address is in any valid user region
        self.text.contains(addr)
            || self.data.contains(addr)
            || self.heap.contains_allocated(addr)
            || self.stack.contains(addr)
    }

    /// Check if an address is valid for heap growth
    ///
    /// Used by page fault handler to determine if a fault in heap
    /// region should trigger lazy allocation.
    pub fn is_valid_heap_address(&self, addr: VirtAddr) -> bool {
        self.heap.contains_valid(addr)
    }
}

impl core::fmt::Debug for AddressSpace {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AddressSpace")
            .field("page_table_root", &self.page_table_root)
            .field("text", &self.text)
            .field("data", &self.data)
            .field("heap_size", &self.heap.size())
            .field("stack", &self.stack)
            .finish()
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // Only clean up userspace address spaces
        // Kernel address spaces use the global kernel page table and should not be freed
        //
        // We detect kernel address spaces by checking if the page_table_root matches
        // the current CR3 (kernel page table). If it does, this is a kernel process
        // and we shouldn't free its page table.
        use x86_64::registers::control::Cr3;
        let (current_pml4_frame, _) = Cr3::read();
        let current_page_table = current_pml4_frame.start_address();

        // If this is the kernel page table, don't free it
        if self.page_table_root == current_page_table {
            return;
        }

        // TODO: Properly free all page tables and mapped frames
        // This requires:
        // 1. Walking the page table hierarchy (PML4 -> PDPT -> PD -> PT)
        // 2. For each mapped page, free the physical frame
        // 3. Free the page table frames themselves
        // 4. Free the PML4 frame
        //
        // For now, we'll just free the PML4 frame to prevent the immediate leak
        // This is not a complete cleanup but prevents the most obvious resource leak

        use crate::memory::{phys, PhysFrame};

        let pml4_frame = PhysFrame::containing_address(self.page_table_root.as_u64());
        phys::free_frame(pml4_frame);

        // Note: This doesn't free child page tables or mapped pages yet
        // A complete implementation would need to:
        // - Walk all valid PML4 entries
        // - For each, walk PDPT entries
        // - For each, walk PD entries
        // - For each, walk PT entries
        // - Free all leaf frames (actual data pages)
        // - Free all page table frames
        //
        // This will be implemented when we add proper page table walking utilities
    }
}

/// Default address space layout constants
///
/// These define the standard memory layout for user processes.
pub mod layout {
    use x86_64::VirtAddr;

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

    /// Get the standard userspace heap region bounds
    pub fn heap_region() -> (VirtAddr, VirtAddr) {
        (VirtAddr::new(USER_HEAP_START), VirtAddr::new(USER_HEAP_MAX))
    }
}
