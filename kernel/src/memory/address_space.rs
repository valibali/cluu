/*
 * Address Space Management (New Implementation)
 *
 * This module implements per-process address spaces using the new physmap-based
 * paging API. Each process has its own page table (PML4 root) providing memory isolation.
 *
 * Key changes from old implementation:
 * - Uses new paging API with physmap access (no CR3 switching hacks)
 * - Explicit kernel space builder that creates fresh page tables
 * - User spaces copy kernel half from the kernel template
 * - All page table manipulation via physmap for clean abstraction
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
 * 0xffff800000000000+       Physmap (direct map of all physical memory)
 * 0xffffffffffe00000        BOOTBOOT info structures
 * 0xffffffffffe02000        Kernel code/data
 * 0xfffffffffff8000000      BOOTBOOT MMIO
 * 0xfffffffffffc000000      BOOTBOOT framebuffer
 * 0xffffffffc0000000        Kernel heap (8 MiB)
 */

use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::PageTableFlags,
};
use crate::bootboot::BOOTBOOT;

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
    /// Build the kernel address space
    ///
    /// This creates a fresh PML4 and maps all kernel regions:
    /// - Kernel code/data (from linker symbols)
    /// - BOOTBOOT structures (info, environment, etc.)
    /// - Framebuffer and MMIO
    /// - Physmap (direct map of all physical memory)
    /// - Kernel heap (will be initialized later)
    ///
    /// This is called during boot to take ownership from BOOTBOOT's page tables.
    ///
    /// # Arguments
    /// * `bootboot_ptr` - Pointer to BOOTBOOT structure
    /// * `kernel_phys_base` - Physical address where kernel is loaded (detected via page table walk)
    /// * `bootboot_phys` - Physical address of BOOTBOOT structure (detected via page table walk)
    ///
    /// # Safety
    /// Must be called during boot, before any other address spaces are created.
    pub unsafe fn build_kernel_space(
        bootboot_ptr: *const BOOTBOOT,
        kernel_phys_base: u64,
        bootboot_phys: u64,
    ) -> Result<Self, &'static str> { unsafe {
        use crate::memory::{paging, physmap};

        log::info!("Building kernel address space...");

        // Allocate new PML4 for kernel
        let pml4_phys = paging::alloc_pml4()?;
        log::info!("Allocated kernel PML4 at {:#x}", pml4_phys.as_u64());

        let bootboot = &*bootboot_ptr;

        // Map kernel code/data
        // The kernel is at virtual KERNEL_OFFSET = 0xffffffffffe02000
        // We need to map it to its physical location
        unsafe extern "C" {
            static __text_start: u8;
            static __bss_end: u8;
            static fb: u8;  // Framebuffer address from linker script
        }

        let kernel_virt_start = &__text_start as *const _ as u64;
        let kernel_virt_end = &__bss_end as *const _ as u64;
        let fb_virt_from_linker = &fb as *const _ as u64;

        // Page-align the kernel mapping
        // The linker symbols might not be page-aligned, but we need to map full pages
        let kernel_virt_start_aligned = kernel_virt_start & !0xfff;  // Round down to page
        let kernel_virt_end_aligned = (kernel_virt_end + 0xfff) & !0xfff;  // Round up to page

        // Calculate the offset from the page-aligned start
        let _offset_in_page = kernel_phys_base & 0xfff;  // Not currently used but kept for reference
        let kernel_phys_start_aligned = kernel_phys_base & !0xfff;  // Round down to page

        let kernel_size = kernel_virt_end_aligned - kernel_virt_start_aligned;

        log::info!(
            "Mapping kernel: virt [{:#x}..{:#x}), size {:#x} bytes, phys base {:#x}",
            kernel_virt_start_aligned,
            kernel_virt_end_aligned,
            kernel_size,
            kernel_phys_start_aligned
        );

        paging::map_range_4k_phys(
            pml4_phys,
            VirtAddr::new(kernel_virt_start_aligned),
            PhysAddr::new(kernel_phys_start_aligned),
            kernel_size,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,  // Kernel code must be executable!
        )?;

        // Map BOOTBOOT info structure
        let bootboot_virt = bootboot_ptr as u64;
        let bootboot_size = core::mem::size_of::<BOOTBOOT>();

        log::info!(
            "Mapping BOOTBOOT info: virt {:#x}, size {:#x}, phys {:#x}",
            bootboot_virt,
            bootboot_size,
            bootboot_phys
        );

        paging::map_range_4k_phys(
            pml4_phys,
            VirtAddr::new(bootboot_virt),
            PhysAddr::new(bootboot_phys),
            bootboot_size as u64,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
        )?;

        // Map framebuffer if present
        let fb_ptr_addr = bootboot.fb_ptr as u64;
        if fb_ptr_addr != 0 {
            // Use BOOTBOOT's fb_size (actual hardware framebuffer size)
            // This accounts for padding and hardware buffering
            let fb_size_bytes = bootboot.fb_size as u64;
            let fb_size = (fb_size_bytes + 0xfff) & !0xfff;  // Round up to 4KB page

            // Detect framebuffer physical address by translating BOOTBOOT's virtual address
            let current_cr3 = paging::get_current_cr3();
            let fb_phys = if let Some(phys) = paging::translate_via_identity(
                current_cr3,
                VirtAddr::new(fb_ptr_addr)
            ) {
                phys.as_u64()
            } else {
                // Fallback: assume direct mapping (common for low addresses)
                log::warn!("Could not translate framebuffer virtual address, using direct mapping");
                fb_ptr_addr
            };

            // Map at the linker script's expected address (from 'fb' symbol)
            // not BOOTBOOT's fb_ptr which might be a different mapping
            log::info!(
                "Mapping framebuffer: virt {:#x}, size {:#x} ({} KB, rounded from {} bytes), phys {:#x}",
                fb_virt_from_linker,
                fb_size,
                fb_size / 1024,
                fb_size_bytes,
                fb_phys
            );

            paging::map_range_4k_phys(
                pml4_phys,
                VirtAddr::new(fb_virt_from_linker),
                PhysAddr::new(fb_phys),
                fb_size,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            )?;
        }

        // Map physmap (direct map of all physical memory)
        // This is the most important mapping - allows access to any physical address
        let max_phys = physmap::max_phys();
        if max_phys == 0 {
            return Err("Physmap not initialized - call physmap::init() first");
        }

        let num_pages = max_phys / 4096;
        log::info!(
            "Mapping physmap: virt {:#x}, phys [0..{:#x}), {} MB ({} pages)",
            physmap::PHYS_MAP_BASE,
            max_phys,
            max_phys / (1024 * 1024),
            num_pages
        );

        log::info!("Starting physmap mapping (this may take a moment)...");

        paging::map_range_4k_phys(
            pml4_phys,
            VirtAddr::new(physmap::PHYS_MAP_BASE),
            PhysAddr::new(0),
            max_phys,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
        )?;

        log::info!("Physmap mapping completed successfully");

        // Kernel heap will be mapped later by heap::init()
        // We just reserve the address space here
        log::info!("Kernel address space built successfully");

        // Verify critical mappings before switching CR3
        log::info!("Verifying page table mappings...");

        // Test 1: Verify kernel code is mapped
        let test_kernel_addr = VirtAddr::new(kernel_virt_start_aligned);
        if let Some((phys, flags)) = paging::translate(pml4_phys, test_kernel_addr) {
            log::info!("  Kernel code mapping OK: virt {:#x} -> phys {:#x}, flags: {:?}",
                test_kernel_addr.as_u64(), phys.as_u64(), flags);
        } else {
            log::error!("  FAILED: Kernel code not mapped at {:#x}!", test_kernel_addr.as_u64());
            return Err("Kernel code not properly mapped");
        }

        // Test 2: Verify physmap is mapped
        let test_physmap_addr = VirtAddr::new(physmap::PHYS_MAP_BASE);
        if let Some((phys, flags)) = paging::translate(pml4_phys, test_physmap_addr) {
            log::info!("  Physmap mapping OK: virt {:#x} -> phys {:#x}, flags: {:?}",
                test_physmap_addr.as_u64(), phys.as_u64(), flags);
        } else {
            log::error!("  FAILED: Physmap not mapped at {:#x}!", test_physmap_addr.as_u64());
            return Err("Physmap not properly mapped");
        }

        // Test 3: CRITICAL - Verify current stack is mapped!
        // If the stack isn't mapped, CR3 switch will triple fault
        let rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nostack, nomem));
        let stack_addr = VirtAddr::new(rsp);

        log::info!("Current stack pointer (RSP): {:#x}", rsp);

        if let Some((phys, flags)) = paging::translate(pml4_phys, stack_addr) {
            log::info!("  Stack mapping OK: virt {:#x} -> phys {:#x}, flags: {:?}",
                stack_addr.as_u64(), phys.as_u64(), flags);
        } else {
            log::error!("  FAILED: Current stack NOT mapped at {:#x}!", stack_addr.as_u64());
            log::error!("  This will cause triple fault on CR3 switch!");
            return Err("Current stack not mapped in new page tables");
        }

        // Test 4: Verify BOOTBOOT structure is mapped
        if let Some((phys, flags)) = paging::translate(pml4_phys, VirtAddr::new(bootboot_virt)) {
            log::info!("  BOOTBOOT mapping OK: virt {:#x} -> phys {:#x}, flags: {:?}",
                bootboot_virt, phys.as_u64(), flags);
        } else {
            log::error!("  FAILED: BOOTBOOT not mapped at {:#x}!", bootboot_virt);
            return Err("BOOTBOOT structure not properly mapped");
        }

        // Test 5: CRITICAL - Verify current instruction pointer (RIP) is mapped!
        // The instruction AFTER the CR3 write must be mapped
        let rip: u64;
        core::arch::asm!("lea {}, [rip]", out(reg) rip, options(nostack, nomem));
        let rip_addr = VirtAddr::new(rip);

        log::info!("Current instruction pointer (RIP): {:#x}", rip);

        // Check if RIP is in the kernel code range
        if rip < kernel_virt_start_aligned || rip >= kernel_virt_end_aligned {
            log::warn!("  RIP {:#x} is OUTSIDE kernel code range [{:#x}..{:#x})!",
                rip, kernel_virt_start_aligned, kernel_virt_end_aligned);
        }

        if let Some((phys, flags)) = paging::translate(pml4_phys, rip_addr) {
            log::info!("  RIP mapping OK: virt {:#x} -> phys {:#x}, flags: {:?}",
                rip_addr.as_u64(), phys.as_u64(), flags);

            // Ensure RIP is executable (not NO_EXECUTE)
            if flags.contains(PageTableFlags::NO_EXECUTE) {
                log::error!("  FAILED: RIP page has NO_EXECUTE flag!");
                return Err("Current instruction pointer page is not executable");
            }
        } else {
            log::error!("  FAILED: Current RIP NOT mapped at {:#x}!", rip_addr.as_u64());
            log::error!("  CR3 switch will fault on instruction fetch!");
            return Err("Current instruction pointer not mapped in new page tables");
        }

        log::info!("Page table verification passed!");

        // Create AddressSpace descriptor
        // Kernel doesn't have user segments
        let null_region = MemoryRegion::new(
            VirtAddr::new(0),
            0,
            PageTableFlags::empty(),
        );

        // Give kernel process a test heap (for sys_brk testing from kernel mode)
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
    }}

    /// Create a new kernel address space (simplified version)
    ///
    /// For kernel threads that share the main kernel page tables.
    /// This just references the current CR3 without creating new tables.
    pub fn new_kernel() -> Self {
        use x86_64::registers::control::Cr3;

        let (current_pml4_frame, _) = Cr3::read();
        let page_table_root = current_pml4_frame.start_address();

        let null_region = MemoryRegion::new(
            VirtAddr::new(0),
            0,
            PageTableFlags::empty(),
        );

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
    /// This allocates a fresh PML4 and copies the kernel half from the
    /// current (kernel) page tables. User regions are initially empty
    /// and will be filled by the ELF loader.
    ///
    /// Steps:
    /// 1. Allocate new PML4
    /// 2. Copy kernel mappings (high half) from current page tables
    /// 3. Set up user region descriptors (initially unmapped)
    pub fn new_user() -> Result<Self, &'static str> {
        use crate::memory::paging;
        use x86_64::registers::control::Cr3;

        // Allocate a new PML4 frame
        let pml4_phys = paging::alloc_pml4()?;

        log::debug!("Allocated new user PML4 at {:#x}", pml4_phys.as_u64());

        // Copy kernel mappings from current page table
        let (current_pml4_frame, _) = Cr3::read();
        let current_pml4_phys = current_pml4_frame.start_address();

        paging::copy_kernel_half(current_pml4_phys, pml4_phys);

        log::debug!("Copied kernel mappings to new user PML4");

        // Initialize user regions (will be filled by ELF loader)
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
    pub fn switch_to(&self) {
        use crate::memory::paging;
        paging::switch_cr3(self.page_table_root);
    }

    /// Check if an address is within user-accessible regions
    ///
    /// Used for validating pointers passed from userspace in syscalls.
    pub fn is_user_accessible(&self, addr: VirtAddr) -> bool {
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
        use x86_64::registers::control::Cr3;
        use crate::memory::{phys, PhysFrame};

        let (current_pml4_frame, _) = Cr3::read();
        let current_page_table = current_pml4_frame.start_address();

        // If this is the kernel page table, don't free it
        if self.page_table_root == current_page_table {
            return;
        }

        // Free the PML4 frame
        let pml4_frame = PhysFrame::containing_address(self.page_table_root.as_u64());
        phys::free_frame(pml4_frame);

        // TODO: Walk and free all child page tables and mapped pages
        // For now we just free the PML4 to prevent the most obvious leak
    }
}

/// Default address space layout constants
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
