//! BOOTBOOT Bootloader Adapter
//!
//! Implements the BootInfoProvider trait for the BOOTBOOT protocol.
//! This is the concrete adapter for BOOTBOOT - other bootloaders will
//! have their own adapters.

use super::info::BootInfoProvider;
use crate::bootboot::{MMapEnt, BOOTBOOT};

/// BOOTBOOT adapter
///
/// Wraps a BOOTBOOT structure and implements the generic BootInfoProvider trait.
pub struct BootbootAdapter {
    bootboot_ptr: *const BOOTBOOT,
}

impl BootbootAdapter {
    /// Create adapter from BOOTBOOT structure pointer
    ///
    /// # Safety
    ///
    /// BOOTBOOT structure must be valid and accessible.
    pub unsafe fn new(bootboot_ptr: *const BOOTBOOT) -> Self {
        Self { bootboot_ptr }
    }

    /// Get reference to BOOTBOOT structure
    fn bootboot(&self) -> &BOOTBOOT {
        unsafe { &*self.bootboot_ptr }
    }

    /// Parse memory map to find maximum physical address
    fn parse_max_physical_address(&self) -> u64 {
        let bootboot = self.bootboot();

        // Calculate number of memory map entries
        let bootboot_size = bootboot.size as usize;
        let mmap_bytes = bootboot_size.saturating_sub(128);
        let mmap_entries = mmap_bytes / core::mem::size_of::<MMapEnt>();

        let mmap_base: *const MMapEnt = &bootboot.mmap as *const MMapEnt;

        let mut max_addr = 0u64;

        for i in 0..mmap_entries {
            let entry = unsafe { &*mmap_base.add(i) };
            let region_ptr = entry.ptr;
            let raw_size = entry.size;
            let region_size = raw_size & !0xF;

            if region_size > 0 {
                let end_addr = region_ptr + region_size;
                if end_addr > max_addr {
                    max_addr = end_addr;
                }
            }
        }

        // Round up to 2MB boundary for easier mapping
        (max_addr + 0x1F_FFFF) & !(0x1F_FFFF)
    }
}

impl BootInfoProvider for BootbootAdapter {
    fn max_physical_address(&self) -> u64 {
        self.parse_max_physical_address()
    }

    fn kernel_physical_range(&self) -> (u64, u64) {
        // Use linker symbols to determine kernel boundaries
        extern "C" {
            static __text_start: u8;
            static __bss_end: u8;
        }

        let kernel_virt_start = unsafe { &__text_start as *const u8 as u64 };
        let kernel_virt_end = unsafe { &__bss_end as *const u8 as u64 };

        // BOOTBOOT maps kernel at high virtual addresses
        // The physical address is encoded in the page tables BOOTBOOT set up
        // We can calculate it by looking at where BOOTBOOT loaded us
        //
        // From BOOTBOOT protocol: kernel is mapped at KERNEL_VIRT_BASE
        // and the physical address is (virt - KERNEL_VIRT_BASE + physical_load_addr)
        //
        // BOOTBOOT doesn't directly tell us the physical load address, but we can
        // calculate it from the virtual address and the known mapping.
        //
        // The BOOTBOOT structure itself is at a known location, so we can use that.
        let _bootboot_virt = self.bootboot_ptr as u64;
        const BOOTBOOT_VIRT: u64 = 0xffffffffffe00000; // From linker script

        // If bootboot is identity mapped, then we know the offset
        // bootboot physical = bootboot_virt - offset
        // From BOOTBOOT spec: bootboot structure is mapped at BOOTBOOT_VIRT
        // and its physical address is wherever it was loaded

        // Actually, simpler approach: use the fact that BOOTBOOT identity maps
        // physical memory linearly at high addresses.
        // kernel_virt_start tells us where we are in virtual space
        // We need to find our physical address by walking the page tables BOOTBOOT set up

        // For now, use the linker script's base address and calculate from there
        const KERNEL_VIRT_BASE: u64 = 0xffffffffffe02000; // From linker script

        // Calculate offset of text_start from base
        let _text_offset = kernel_virt_start - KERNEL_VIRT_BASE;

        // The kernel physical start needs to be determined from BOOTBOOT
        // BOOTBOOT loads the kernel and sets up page tables
        // We can read the physical address from CR3 and walk to find our physical address

        // Simpler solution: Use a known physical address calculation
        // The kernel is loaded at physical address that BOOTBOOT chose
        // We need to read this from the page tables
        use x86_64::registers::control::Cr3;
        use x86_64::structures::paging::PageTable;

        let (pml4_frame, _) = Cr3::read();
        let pml4_phys = pml4_frame.start_address().as_u64();

        // PML4 is identity mapped by BOOTBOOT, so we can access it
        let pml4 = unsafe { &*(pml4_phys as *const PageTable) };

        // Get PML4 entry for kernel (index 511)
        let pml4e = pml4[511].addr().as_u64();
        let pdpt = unsafe { &*(pml4e as *const PageTable) };

        // Get PDPT entry for kernel (index 511)
        let pdpte = pdpt[511].addr().as_u64();
        let pd = unsafe { &*(pdpte as *const PageTable) };

        // Get PD entry for kernel (index 511)
        let pde = pd[511].addr().as_u64();

        // Check if it's a huge page (bit 7 set) or PT pointer
        let _pde_flags = pd[511].flags();
        let is_huge_page = (pde & 0x80) != 0;

        let kernel_phys_base = if is_huge_page {
            // 2MB huge page - extract 2MB-aligned physical address
            pde & !0x1F_FFFF
        } else {
            // 4KB pages - need to walk to PT and read the first PTE
            let pt = unsafe { &*(pde as *const PageTable) };

            // Calculate which PT entry corresponds to __text_start
            let pt_idx = ((kernel_virt_start >> 12) & 0x1FF) as usize;
            let pte = pt[pt_idx].addr().as_u64();

            // PTE contains 4KB-aligned physical address
            pte & !0xFFF
        };

        // kernel_phys_base now points to the physical address of __text_start
        let kernel_phys_start = kernel_phys_base;
        let kernel_phys_end = kernel_phys_start + (kernel_virt_end - kernel_virt_start);

        (kernel_phys_start, kernel_phys_end)
    }

    fn initrd_location(&self) -> Option<(u64, u64)> {
        let bootboot = self.bootboot();
        let initrd_ptr = bootboot.initrd_ptr;
        let initrd_size = bootboot.initrd_size;

        if initrd_size > 0 {
            Some((initrd_ptr, initrd_size))
        } else {
            None
        }
    }

    fn framebuffer_location(&self) -> Option<(u64, u64)> {
        let bootboot = self.bootboot();
        let fb_ptr = bootboot.fb_ptr as u64;
        let fb_size = bootboot.fb_size as u64;

        if fb_size > 0 && fb_ptr != 0 {
            Some((fb_ptr, fb_size))
        } else {
            None
        }
    }

    fn info_structure_location(&self) -> u64 {
        self.bootboot_ptr as u64
    }
}
