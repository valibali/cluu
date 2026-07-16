//! BlkRequestQueue — the in-process queue of LBA reads/writes.
//!
//! Owns one Virtqueue (queue 0). Submitted requests are tracked by
//! their virtqueue cookie (which packs (session_id << 32) | request_id).
//! Completions are drained from the used ring on IRQ wake.

use crate::protocol::{VirtioBlkReqHeader, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT};
use cluu_virtio_core::dma::{DmaPool, DmaRegion};
use cluu_virtio_core::transport::Transport;
use cluu_virtio_core::virtqueue::{
    Virtqueue, VRingDesc, VRING_DESC_F_INDIRECT, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE,
};
use alloc::vec::Vec;
use libcluu::{Error, Result};

const INDIRECT_ENTRIES_PER_PAGE: usize = 4096 / core::mem::size_of::<VRingDesc>();

/// Per-request bookkeeping while a request is in flight: the DMA region
/// holding the on-the-wire header + status byte for THIS request.
pub struct InflightSlot {
    pub cookie: u64,
    pub header_region: DmaRegion,
    pub status_region: DmaRegion,
    pub indirect_regions: Vec<DmaRegion>,
}

pub struct BlkRequestQueue<T: Transport> {
    pub transport: T,
    pub vq: Virtqueue,
    pub pool: DmaPool,
    pub in_flight: Vec<InflightSlot>,
    free_slots: Vec<(DmaRegion, DmaRegion)>,
    free_indirect: Vec<DmaRegion>,
}

impl<T: Transport> BlkRequestQueue<T> {
    pub fn new(mut transport: T, mut pool: DmaPool, queue_size: u16) -> Result<Self> {
        let vq = Virtqueue::new(&mut pool, queue_size)?;
        transport.configure_queue(0, &vq)?;
        Ok(Self {
            transport,
            vq,
            pool,
            in_flight: Vec::new(),
            free_slots: Vec::new(),
            free_indirect: Vec::new(),
        })
    }

    /// Submit a read of `total_bytes` from `lba` into the caller-provided
    /// physical pages `page_phys[..]`. `cookie` is opaque routing data.
    /// Returns Ok(()) and `notify` is the caller's responsibility to issue
    /// after a batch of submits to amortize the MMIO exit.
    ///
    /// Descriptor chain shape:
    ///   [ header(OUT, len=16) -> page0(WRITE) -> ... -> pageN(WRITE) -> status(WRITE, 1) ]
    pub fn submit_read(
        &mut self,
        lba: u64,
        page_phys: &[u64],
        total_bytes: usize,
        cookie: u64,
    ) -> Result<()> {
        if page_phys.is_empty() {
            return Err(Error::InvalidArgument);
        }

        let (header_region, status_region) = match self.free_slots.pop() {
            Some(pair) => pair,
            None => {
                let h = self.pool.alloc(16, 16)?;
                let s = self.pool.alloc(1, 1)?;
                (h, s)
            }
        };

        unsafe {
            let h = header_region.virt as *mut VirtioBlkReqHeader;
            (*h).type_ = VIRTIO_BLK_T_IN;
            (*h).reserved = 0;
            (*h).sector = lba;
        }
        unsafe {
            *(status_region.virt as *mut u8) = 0xFF;
        }

        let qsize = self.vq.queue_size as usize;
        let direct_descs = page_phys.len() + 2;

        if direct_descs <= qsize {
            self.submit_read_direct(page_phys, total_bytes, cookie, header_region, status_region)
        } else {
            self.submit_read_indirect(page_phys, total_bytes, cookie, header_region, status_region)
        }
    }

    fn submit_read_direct(
        &mut self,
        page_phys: &[u64],
        total_bytes: usize,
        cookie: u64,
        header_region: DmaRegion,
        status_region: DmaRegion,
    ) -> Result<()> {
        let n_descs = (page_phys.len() + 2) as u16;
        let chain = match self.vq.alloc_chain(n_descs) {
            Some(c) => c,
            None => {
                self.free_slots.push((header_region, status_region));
                return Err(Error::Busy);
            }
        };

        let descs = self.collect_chain_indices(chain.head, n_descs);
        for (i, &didx) in descs.iter().enumerate() {
            let is_last = i == descs.len() - 1;
            if i == 0 {
                let next_link = if is_last { 0 } else { descs[i + 1] };
                let flags = if is_last { 0 } else { VRING_DESC_F_NEXT };
                self.vq.desc_set(didx, header_region.phys, 16, flags, next_link);
            } else if i == descs.len() - 1 {
                self.vq.desc_set(didx, status_region.phys, 1, VRING_DESC_F_WRITE, 0);
            } else {
                let page_idx = i - 1;
                let bytes_in_page = if page_idx == page_phys.len() - 1 {
                    total_bytes - page_idx * 4096
                } else {
                    4096
                };
                let next_link = descs[i + 1];
                self.vq.desc_set(
                    didx,
                    page_phys[page_idx],
                    bytes_in_page as u32,
                    VRING_DESC_F_NEXT | VRING_DESC_F_WRITE,
                    next_link,
                );
            }
        }

        self.vq.submit(chain, cookie);
        self.in_flight.push(InflightSlot {
            cookie,
            header_region,
            status_region,
            indirect_regions: Vec::new(),
        });
        Ok(())
    }

    fn submit_read_indirect(
        &mut self,
        page_phys: &[u64],
        total_bytes: usize,
        cookie: u64,
        header_region: DmaRegion,
        status_region: DmaRegion,
    ) -> Result<()> {
        let num_indirect_tables = page_phys.len().div_ceil(INDIRECT_ENTRIES_PER_PAGE);
        let main_descs = (2 + num_indirect_tables) as u16;

        let chain = match self.vq.alloc_chain(main_descs) {
            Some(c) => c,
            None => {
                self.free_slots.push((header_region, status_region));
                return Err(Error::Busy);
            }
        };

        let mut indirect_regions: Vec<DmaRegion> = Vec::with_capacity(num_indirect_tables);
        for _ in 0..num_indirect_tables {
            let region = match self.free_indirect.pop() {
                Some(r) => r,
                None => self.pool.alloc(4096, 4096)?,
            };
            indirect_regions.push(region);
        }

        for (table_idx, table_region) in indirect_regions.iter().enumerate() {
            let table_ptr = table_region.virt as *mut VRingDesc;
            let start = table_idx * INDIRECT_ENTRIES_PER_PAGE;
            let end = (start + INDIRECT_ENTRIES_PER_PAGE).min(page_phys.len());
            let count = end - start;

            unsafe {
                core::ptr::write_bytes(table_ptr as *mut u8, 0, 4096);
            }

            for j in 0..count {
                let page_idx = start + j;
                let bytes_in_page = if page_idx == page_phys.len() - 1 {
                    total_bytes - page_idx * 4096
                } else {
                    4096
                };
                let is_last_in_table = j == count - 1;
                unsafe {
                    let entry = table_ptr.add(j);
                    (*entry).addr = page_phys[page_idx];
                    (*entry).len = bytes_in_page as u32;
                    (*entry).flags = VRING_DESC_F_WRITE
                        | if is_last_in_table { 0 } else { VRING_DESC_F_NEXT };
                    (*entry).next = if is_last_in_table { 0 } else { (j + 1) as u16 };
                }
            }
        }

        let descs = self.collect_chain_indices(chain.head, main_descs);
        for (i, &didx) in descs.iter().enumerate() {
            let is_last = i == descs.len() - 1;
            if i == 0 {
                let next_link = descs[i + 1];
                self.vq.desc_set(didx, header_region.phys, 16, VRING_DESC_F_NEXT, next_link);
            } else if is_last {
                self.vq.desc_set(didx, status_region.phys, 1, VRING_DESC_F_WRITE, 0);
            } else {
                let table_idx = i - 1;
                let table_region = &indirect_regions[table_idx];
                let table_entries = {
                    let start = table_idx * INDIRECT_ENTRIES_PER_PAGE;
                    let end = (start + INDIRECT_ENTRIES_PER_PAGE).min(page_phys.len());
                    end - start
                };
                let table_bytes = (table_entries * core::mem::size_of::<VRingDesc>()) as u32;
                let next_link = if is_last { 0 } else { descs[i + 1] };
                self.vq.desc_set(
                    didx,
                    table_region.phys,
                    table_bytes,
                    VRING_DESC_F_INDIRECT | VRING_DESC_F_NEXT,
                    next_link,
                );
            }
        }

        self.vq.submit(chain, cookie);
        self.in_flight.push(InflightSlot {
            cookie,
            header_region,
            status_region,
            indirect_regions,
        });
        Ok(())
    }

    /// Submit a write of `total_bytes` from caller-provided physical pages
    /// `page_phys[..]` to `lba`. `cookie` is opaque routing data. `notify` is
    /// the caller's responsibility after a batch of submits.
    ///
    /// Descriptor chain shape:
    ///   [ header(OUT, len=16) -> page0(OUT) -> ... -> pageN(OUT) -> status(WRITE, 1) ]
    /// Buffer pages are device-readable (no VRING_DESC_F_WRITE on data descs).
    pub fn submit_write(
        &mut self,
        lba: u64,
        page_phys: &[u64],
        total_bytes: usize,
        cookie: u64,
    ) -> Result<()> {
        if page_phys.is_empty() {
            return Err(Error::InvalidArgument);
        }

        let (header_region, status_region) = match self.free_slots.pop() {
            Some(pair) => pair,
            None => {
                let h = self.pool.alloc(16, 16)?;
                let s = self.pool.alloc(1, 1)?;
                (h, s)
            }
        };

        unsafe {
            let h = header_region.virt as *mut VirtioBlkReqHeader;
            (*h).type_ = VIRTIO_BLK_T_OUT;
            (*h).reserved = 0;
            (*h).sector = lba;
        }
        unsafe {
            *(status_region.virt as *mut u8) = 0xFF;
        }

        let qsize = self.vq.queue_size as usize;
        let direct_descs = page_phys.len() + 2;

        if direct_descs <= qsize {
            self.submit_write_direct(page_phys, total_bytes, cookie, header_region, status_region)
        } else {
            self.submit_write_indirect(page_phys, total_bytes, cookie, header_region, status_region)
        }
    }

    fn submit_write_direct(
        &mut self,
        page_phys: &[u64],
        total_bytes: usize,
        cookie: u64,
        header_region: DmaRegion,
        status_region: DmaRegion,
    ) -> Result<()> {
        let n_descs = (page_phys.len() + 2) as u16;
        let chain = match self.vq.alloc_chain(n_descs) {
            Some(c) => c,
            None => {
                self.free_slots.push((header_region, status_region));
                return Err(Error::Busy);
            }
        };

        let descs = self.collect_chain_indices(chain.head, n_descs);
        for (i, &didx) in descs.iter().enumerate() {
            let is_last = i == descs.len() - 1;
            if i == 0 {
                let next_link = if is_last { 0 } else { descs[i + 1] };
                let flags = if is_last { 0 } else { VRING_DESC_F_NEXT };
                self.vq.desc_set(didx, header_region.phys, 16, flags, next_link);
            } else if i == descs.len() - 1 {
                self.vq.desc_set(didx, status_region.phys, 1, VRING_DESC_F_WRITE, 0);
            } else {
                let page_idx = i - 1;
                let bytes_in_page = if page_idx == page_phys.len() - 1 {
                    total_bytes - page_idx * 4096
                } else {
                    4096
                };
                let next_link = descs[i + 1];
                self.vq.desc_set(
                    didx,
                    page_phys[page_idx],
                    bytes_in_page as u32,
                    VRING_DESC_F_NEXT,
                    next_link,
                );
            }
        }

        self.vq.submit(chain, cookie);
        self.in_flight.push(InflightSlot {
            cookie,
            header_region,
            status_region,
            indirect_regions: Vec::new(),
        });
        Ok(())
    }

    fn submit_write_indirect(
        &mut self,
        page_phys: &[u64],
        total_bytes: usize,
        cookie: u64,
        header_region: DmaRegion,
        status_region: DmaRegion,
    ) -> Result<()> {
        let num_indirect_tables = page_phys.len().div_ceil(INDIRECT_ENTRIES_PER_PAGE);
        let main_descs = (2 + num_indirect_tables) as u16;

        let chain = match self.vq.alloc_chain(main_descs) {
            Some(c) => c,
            None => {
                self.free_slots.push((header_region, status_region));
                return Err(Error::Busy);
            }
        };

        let mut indirect_regions: Vec<DmaRegion> = Vec::with_capacity(num_indirect_tables);
        for _ in 0..num_indirect_tables {
            let region = match self.free_indirect.pop() {
                Some(r) => r,
                None => self.pool.alloc(4096, 4096)?,
            };
            indirect_regions.push(region);
        }

        for (table_idx, table_region) in indirect_regions.iter().enumerate() {
            let table_ptr = table_region.virt as *mut VRingDesc;
            let start = table_idx * INDIRECT_ENTRIES_PER_PAGE;
            let end = (start + INDIRECT_ENTRIES_PER_PAGE).min(page_phys.len());
            let count = end - start;

            unsafe {
                core::ptr::write_bytes(table_ptr as *mut u8, 0, 4096);
            }

            for j in 0..count {
                let page_idx = start + j;
                let bytes_in_page = if page_idx == page_phys.len() - 1 {
                    total_bytes - page_idx * 4096
                } else {
                    4096
                };
                let is_last_in_table = j == count - 1;
                unsafe {
                    let entry = table_ptr.add(j);
                    (*entry).addr = page_phys[page_idx];
                    (*entry).len = bytes_in_page as u32;
                    (*entry).flags = if is_last_in_table { 0 } else { VRING_DESC_F_NEXT };
                    (*entry).next = if is_last_in_table { 0 } else { (j + 1) as u16 };
                }
            }
        }

        let descs = self.collect_chain_indices(chain.head, main_descs);
        for (i, &didx) in descs.iter().enumerate() {
            let is_last = i == descs.len() - 1;
            if i == 0 {
                let next_link = descs[i + 1];
                self.vq.desc_set(didx, header_region.phys, 16, VRING_DESC_F_NEXT, next_link);
            } else if is_last {
                self.vq.desc_set(didx, status_region.phys, 1, VRING_DESC_F_WRITE, 0);
            } else {
                let table_idx = i - 1;
                let table_region = &indirect_regions[table_idx];
                let table_entries = {
                    let start = table_idx * INDIRECT_ENTRIES_PER_PAGE;
                    let end = (start + INDIRECT_ENTRIES_PER_PAGE).min(page_phys.len());
                    end - start
                };
                let table_bytes = (table_entries * core::mem::size_of::<VRingDesc>()) as u32;
                let next_link = if is_last { 0 } else { descs[i + 1] };
                self.vq.desc_set(
                    didx,
                    table_region.phys,
                    table_bytes,
                    VRING_DESC_F_INDIRECT | VRING_DESC_F_NEXT,
                    next_link,
                );
            }
        }

        self.vq.submit(chain, cookie);
        self.in_flight.push(InflightSlot {
            cookie,
            header_region,
            status_region,
            indirect_regions,
        });
        Ok(())
    }

    /// Issue a single device notify covering all submits since the last call.
    pub fn notify(&self) {
        self.transport.notify(0);
    }

    /// Drain used-ring entries. Returns Vec<(cookie, status_byte, len)>.
    /// Per-request DMA regions are returned to `free_slots` for reuse.
    pub fn drain_completions(&mut self) -> Vec<(u64, u8, u32)> {
        let mut out = Vec::new();
        while let Some((cookie, len)) = self.vq.pop_used() {
            let pos = match self.in_flight.iter().position(|s| s.cookie == cookie) {
                Some(p) => p,
                None => continue,
            };
            let slot = self.in_flight.swap_remove(pos);
            let status = unsafe { *(slot.status_region.virt as *const u8) };
            out.push((cookie, status, len));
            self.free_slots.push((slot.header_region, slot.status_region));
            for region in slot.indirect_regions {
                self.free_indirect.push(region);
            }
        }
        out
    }

    pub fn free_capacity(&self) -> u16 {
        self.vq.free_capacity()
    }

    fn collect_chain_indices(&self, head: u16, n: u16) -> Vec<u16> {
        // alloc_chain pulls n entries via the in-table NEXT field; we walked
        // them already to figure out tail. Re-walk to give descriptors
        // ordered list. This matches what alloc_chain's free-list traversal
        // does.
        let mut out = Vec::with_capacity(n as usize);
        let mut cur = head;
        for _ in 0..n {
            out.push(cur);
            // We need the original link (alloc_chain disconnected the tail)
            // — read the desc table directly.
            let next = unsafe {
                let p = (self.vq.desc_region.virt
                    as *const cluu_virtio_core::virtqueue::VRingDesc)
                    .add(cur as usize);
                (*p).next
            };
            cur = next;
        }
        out
    }
}
