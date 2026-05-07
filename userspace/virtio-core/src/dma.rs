//! DMA-pinned regions for descriptor tables, headers, status bytes.
//!
//! Single pre-allocated virtual region carved up by a bump pointer; lifetime
//! is the driver's lifetime. Each handed-out `DmaRegion` carries both its
//! virtual address (for CPU access) and its physical address (for device
//! descriptors). `phys` is resolved once at allocation time via the kernel
//! `virt_to_phys` syscall and cached — the underlying frames are pinned for
//! the driver's lifetime so the cached phys never goes stale.

use alloc::vec::Vec;
use libcluu::syscall::{space_map_range, virt_to_phys};
use libcluu::{Error, Result};

const DMA_REGION_FLAGS: usize = 0x03; // R+W

pub struct DmaPool {
    base_va: usize,
    size: usize,
    next_offset: usize,
    space_token: usize,
    page_phys: Vec<u64>, // phys per 4KB page
}

#[derive(Copy, Clone, Debug)]
pub struct DmaRegion {
    pub virt: usize,
    pub phys: u64,
    pub len: usize,
}

impl DmaPool {
    /// Allocate `pages * 4096` bytes of pinned virtual range and resolve
    /// each page's physical address. The region is handed out in
    /// `align`-aligned subregions by `alloc()`.
    pub fn new(space_token: usize, base_va: usize, pages: usize) -> Result<Self> {
        space_map_range(space_token, base_va, 0, DMA_REGION_FLAGS, pages, 0)?;
        let mut page_phys = Vec::with_capacity(pages);
        for i in 0..pages {
            let va = base_va + i * 4096;
            let phys = virt_to_phys(space_token, va)?;
            page_phys.push(phys as u64);
        }
        Ok(Self {
            base_va,
            size: pages * 4096,
            next_offset: 0,
            space_token,
            page_phys,
        })
    }

    /// Carve out a `len`-byte subregion aligned to `align` (must be power of 2).
    /// Returns Err(Overflow) if there isn't enough remaining space. The caller
    /// must not span a 4 KiB page boundary unless `len <= 4096` and aligned
    /// such that the region fits in one page (most descriptor tables and
    /// per-request header/status structs are tiny — far below 4 KiB).
    pub fn alloc(&mut self, len: usize, align: usize) -> Result<DmaRegion> {
        if !align.is_power_of_two() {
            return Err(Error::InvalidArgument);
        }
        let aligned = (self.next_offset + align - 1) & !(align - 1);
        if aligned + len > self.size {
            return Err(Error::Overflow);
        }
        // Forbid a region crossing a 4 KiB page boundary so the cached
        // page-phys is unambiguous for that region.
        let page_idx = aligned / 4096;
        let last_byte_page = (aligned + len - 1) / 4096;
        if page_idx != last_byte_page {
            // Skip to next page boundary and retry once.
            let new_offset = (page_idx + 1) * 4096;
            if new_offset + len > self.size {
                return Err(Error::Overflow);
            }
            self.next_offset = new_offset;
            return self.alloc(len, align);
        }
        let virt = self.base_va + aligned;
        let phys_base = self.page_phys[page_idx];
        let intra_page_offset = (aligned % 4096) as u64;
        self.next_offset = aligned + len;
        Ok(DmaRegion {
            virt,
            phys: phys_base + intra_page_offset,
            len,
        })
    }

    /// Resolve a previously-allocated virt back to its phys. O(1).
    pub fn phys_of(&self, virt: usize) -> Option<u64> {
        if virt < self.base_va || virt >= self.base_va + self.size {
            return None;
        }
        let offset = virt - self.base_va;
        let page_idx = offset / 4096;
        Some(self.page_phys[page_idx] + (offset % 4096) as u64)
    }

    pub fn space_token(&self) -> usize {
        self.space_token
    }
}
