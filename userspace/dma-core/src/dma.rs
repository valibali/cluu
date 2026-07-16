use alloc::vec::Vec;
use libcluu::syscall::{space_map_range, virt_to_phys};
use libcluu::{Error, Result};

const DMA_REGION_FLAGS: usize = 0x03;
const PAGE_SIZE: usize = 4096;

pub struct DmaPool {
    base_va: usize,
    size: usize,
    next_offset: usize,
    space_token: usize,
    page_phys: Vec<u64>,
}

#[derive(Copy, Clone, Debug)]
pub struct DmaRegion {
    pub virt: usize,
    pub phys: u64,
    pub len: usize,
}

impl DmaPool {
    pub fn new(space_token: usize, base_va: usize, pages: usize) -> Result<Self> {
        space_map_range(space_token, base_va, 0, DMA_REGION_FLAGS, pages, 0)?;
        let mut page_phys = Vec::with_capacity(pages);
        for i in 0..pages {
            let va = base_va + i * PAGE_SIZE;
            let phys = virt_to_phys(space_token, va)?;
            page_phys.push(phys as u64);
        }
        Ok(Self {
            base_va,
            size: pages * PAGE_SIZE,
            next_offset: 0,
            space_token,
            page_phys,
        })
    }

    pub fn alloc(&mut self, len: usize, align: usize) -> Result<DmaRegion> {
        if !align.is_power_of_two() {
            return Err(Error::InvalidArgument);
        }
        if len > PAGE_SIZE {
            return Err(Error::Overflow);
        }
        let mut aligned = (self.next_offset + align - 1) & !(align - 1);
        if aligned + len > self.size {
            return Err(Error::Overflow);
        }
        let mut page_idx = aligned / PAGE_SIZE;
        let last_byte_page = (aligned + len - 1) / PAGE_SIZE;
        if page_idx != last_byte_page {
            aligned = (page_idx + 1) * PAGE_SIZE;
            if aligned + len > self.size {
                return Err(Error::Overflow);
            }
            page_idx = aligned / PAGE_SIZE;
        }
        let virt = self.base_va + aligned;
        let phys_base = self.page_phys[page_idx];
        let intra_page_offset = (aligned % PAGE_SIZE) as u64;
        self.next_offset = aligned + len;
        Ok(DmaRegion {
            virt,
            phys: phys_base + intra_page_offset,
            len,
        })
    }

    pub fn alloc_contiguous(&mut self, pages: usize) -> Result<DmaRegion> {
        let len = pages * PAGE_SIZE;
        let aligned = (self.next_offset + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        if aligned + len > self.size {
            return Err(Error::Overflow);
        }
        let page_idx = aligned / PAGE_SIZE;
        let virt = self.base_va + aligned;
        let phys_base = self.page_phys[page_idx];
        self.next_offset = aligned + len;
        Ok(DmaRegion {
            virt,
            phys: phys_base,
            len,
        })
    }

    pub fn phys_of(&self, virt: usize) -> Option<u64> {
        if virt < self.base_va || virt >= self.base_va + self.size {
            return None;
        }
        let offset = virt - self.base_va;
        let page_idx = offset / PAGE_SIZE;
        Some(self.page_phys[page_idx] + (offset % PAGE_SIZE) as u64)
    }

    pub fn space_token(&self) -> usize {
        self.space_token
    }
}
