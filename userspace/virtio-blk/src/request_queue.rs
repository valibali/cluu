//! BlkRequestQueue — the in-process queue of LBA reads/writes.
//!
//! Owns one Virtqueue (queue 0). Submitted requests are tracked by
//! their virtqueue cookie (which packs (session_id << 32) | request_id).
//! Completions are drained from the used ring on IRQ wake.

use crate::protocol::{VirtioBlkReqHeader, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT};
use cluu_virtio_core::dma::{DmaPool, DmaRegion};
use cluu_virtio_core::transport::Transport;
use cluu_virtio_core::virtqueue::{Virtqueue, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};
use alloc::vec::Vec;
use libcluu::{Error, Result};

/// Per-request bookkeeping while a request is in flight: the DMA region
/// holding the on-the-wire header + status byte for THIS request.
pub struct InflightSlot {
    pub cookie: u64,
    pub header_region: DmaRegion,
    pub status_region: DmaRegion,
}

pub struct BlkRequestQueue<T: Transport> {
    pub transport: T,
    pub vq: Virtqueue,
    pub pool: DmaPool,
    pub in_flight: Vec<InflightSlot>,
    /// Recycled (header, status) DMA region pairs returned from completed
    /// requests. Pre-allocated regions are reused before tapping the pool.
    free_slots: Vec<(DmaRegion, DmaRegion)>,
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

        // Acquire (header, status) DMA pair: prefer the recycled free-list
        // before tapping the bump pool. This bounds steady-state pool usage
        // to high-water-mark in_flight depth, not lifetime request count.
        let (header_region, status_region) = match self.free_slots.pop() {
            Some(pair) => pair,
            None => {
                let h = self.pool.alloc(16, 16)?;
                let s = self.pool.alloc(1, 1)?;
                (h, s)
            }
        };

        let n_descs = (page_phys.len() + 2) as u16; // header + N + status
        let chain = match self.vq.alloc_chain(n_descs) {
            Some(c) => c,
            None => {
                // Return the regions to the free-list so the next request
                // can reuse them; they outlive request lifetimes.
                self.free_slots.push((header_region, status_region));
                return Err(Error::Busy);
            }
        };

        // Fill header.
        unsafe {
            let h = header_region.virt as *mut VirtioBlkReqHeader;
            (*h).type_ = VIRTIO_BLK_T_IN;
            (*h).reserved = 0;
            (*h).sector = lba;
        }
        unsafe {
            *(status_region.virt as *mut u8) = 0xFF; // sentinel; device overwrites
        }

        // Walk the chain to fill descriptors.
        // Build chain links: every desc except the last has NEXT.
        let descs = self.collect_chain_indices(chain.head, n_descs);
        for (i, &didx) in descs.iter().enumerate() {
            let is_last = i == descs.len() - 1;
            if i == 0 {
                // Header: device reads, driver writes (default == OUT_FROM_DRIVER).
                let next_link = if is_last { 0 } else { descs[i + 1] };
                let flags = if is_last { 0 } else { VRING_DESC_F_NEXT };
                self.vq
                    .desc_set(didx, header_region.phys, 16, flags, next_link);
            } else if i == descs.len() - 1 {
                // Status: device writes.
                self.vq
                    .desc_set(didx, status_region.phys, 1, VRING_DESC_F_WRITE, 0);
            } else {
                // Buffer pages: device writes (this is a read request, so
                // the buffer is filled BY the device).
                let page_idx = i - 1;
                let bytes_in_page = if page_idx == page_phys.len() - 1 {
                    let rem = total_bytes - page_idx * 4096;
                    rem
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

        let n_descs = (page_phys.len() + 2) as u16;
        let chain = match self.vq.alloc_chain(n_descs) {
            Some(c) => c,
            None => {
                self.free_slots.push((header_region, status_region));
                return Err(Error::Busy);
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

        let descs = self.collect_chain_indices(chain.head, n_descs);
        for (i, &didx) in descs.iter().enumerate() {
            let is_last = i == descs.len() - 1;
            if i == 0 {
                // Header: device reads, OUT (no WRITE flag).
                let next_link = if is_last { 0 } else { descs[i + 1] };
                let flags = if is_last { 0 } else { VRING_DESC_F_NEXT };
                self.vq
                    .desc_set(didx, header_region.phys, 16, flags, next_link);
            } else if i == descs.len() - 1 {
                // Status: device writes.
                self.vq
                    .desc_set(didx, status_region.phys, 1, VRING_DESC_F_WRITE, 0);
            } else {
                // Buffer pages: device READS them — no VRING_DESC_F_WRITE.
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
            // Find the in-flight slot for this cookie to read its status.
            let pos = match self.in_flight.iter().position(|s| s.cookie == cookie) {
                Some(p) => p,
                None => continue,
            };
            let slot = self.in_flight.swap_remove(pos);
            let status = unsafe { *(slot.status_region.virt as *const u8) };
            out.push((cookie, status, len));
            // Recycle the DMA regions back into the free-list.
            self.free_slots.push((slot.header_region, slot.status_region));
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
