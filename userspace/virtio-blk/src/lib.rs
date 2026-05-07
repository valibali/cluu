//! Virtio-blk block device driver for CLUU.
//!
//! This module provides the core virtio-blk implementation that implements
//! the `BlockDevice` trait from libcluu. Filesystem plugins (like ext2) can
//! use this trait directly without IPC overhead.

#![no_std]

extern crate alloc;

pub mod pci;
pub mod protocol;
pub mod request_queue;
pub mod session;
pub mod virtio;
pub mod virtqueue;

use libcluu::fs::BlockDevice;
use libcluu::Result;
use spin::Mutex;

pub use virtio::VirtioBlkDevice;

use crate::request_queue::BlkRequestQueue;
use cluu_virtio_core::transport::{ModernPciTransport, Transport};

/// BlockDevice adapter built on the new virtio-core stack.
///
/// Waits on an IRQ-attached endpoint for ring completions in `read_bytes`.
/// T5.7 will allow multiple in-flight requests at the IPC boundary; for now
/// requests are strictly serialized (one in flight, single fixed cookie)
/// and use a pre-mapped scratch buffer for DMA so we don't have to resolve
/// `virt_to_phys` on freshly allocated `Vec` memory (which has no alignment
/// guarantee).
pub struct ModernBlkAdapter {
    /// Public so the BLK_OPEN_SESSION/SUBMIT/CLOSE handlers in `main.rs`
    /// can lock the request queue directly (T6.3 dispatch is synchronous
    /// and shares the mutex with the FS path's `read_bytes`).
    pub inner: Mutex<BlkRequestQueue<ModernPciTransport>>,
    capacity_sectors: u64,
    sector_size_bytes: usize,
    scratch_base: usize,
    scratch_pages: usize,
    /// Public so the BLK_SUBMIT handler in `main.rs` can `recv_any` on the
    /// same IRQ-attached endpoint that `read_bytes` waits on. The dispatch
    /// is single-threaded and the BlkRequestQueue mutex is held end-to-end
    /// during a submit, so no concurrent recv races are possible today.
    pub irq_endpoint: usize,
}

impl ModernBlkAdapter {
    pub fn new(
        bq: BlkRequestQueue<ModernPciTransport>,
        capacity_sectors: u64,
        sector_size_bytes: usize,
        scratch_base: usize,
        scratch_pages: usize,
        irq_endpoint: usize,
    ) -> Self {
        Self {
            inner: Mutex::new(bq),
            capacity_sectors,
            sector_size_bytes,
            scratch_base,
            scratch_pages,
            irq_endpoint,
        }
    }
}

impl BlockDevice for ModernBlkAdapter {
    fn read_bytes(&self, offset: u64, buf_out: &mut [u8]) -> Result<usize> {
        if buf_out.is_empty() {
            return Ok(0);
        }
        let mut bq = self.inner.lock();

        let sector_size = self.sector_size_bytes as u64;
        let start_sector = offset / sector_size;
        let sector_offset = (offset % sector_size) as usize;
        let end_byte = offset + buf_out.len() as u64;
        let end_sector = end_byte.div_ceil(sector_size);
        let sector_count = end_sector - start_sector;
        let total_bytes = (sector_count as usize) * self.sector_size_bytes;

        if total_bytes > self.scratch_pages * 4096 {
            return Err(libcluu::Error::BufferTooSmall);
        }

        // Resolve the scratch buffer's physical pages each time. The mapping
        // was pinned at init via space_map_range so phys is stable, but we
        // don't currently cache it on the adapter.
        let space_token = bq.pool.space_token();
        let n_pages = total_bytes.div_ceil(4096);
        let mut pages: alloc::vec::Vec<u64> = alloc::vec::Vec::with_capacity(n_pages);
        for i in 0..n_pages {
            let va = self.scratch_base + i * 4096;
            let phys = libcluu::syscall::virt_to_phys(space_token, va)? as u64;
            pages.push(phys);
        }

        let cookie: u64 = 1; // single-in-flight; T5.7 will route by real cookie
        bq.submit_read(start_sector, &pages, total_bytes, cookie)?;
        bq.notify();

        // Spin-poll the used ring (debug fallback while IRQ delivery is
        // being verified). Yields after a chunk of spins so other threads
        // make progress.
        let mut spins = 0u64;
        loop {
            let completions = bq.drain_completions();
            for (got, status, _len) in completions {
                if got == cookie {
                    if status != 0 {
                        return Err(libcluu::Error::InvalidState);
                    }
                    let copy_len = buf_out.len().min(total_bytes - sector_offset);
                    let scratch = unsafe {
                        core::slice::from_raw_parts(
                            self.scratch_base as *const u8,
                            total_bytes,
                        )
                    };
                    buf_out[..copy_len].copy_from_slice(
                        &scratch[sector_offset..sector_offset + copy_len],
                    );
                    return Ok(copy_len);
                }
            }
            spins += 1;
            if spins == 10_000_000 {
                let _ = libcluu::debug_print(
                    "virtio-blk/read_bytes: spun 10M without completion — bailing",
                );
                return Err(libcluu::Error::Timeout);
            }
            if spins.is_multiple_of(1024) {
                let _ = libcluu::syscall::yield_cpu();
            }
            core::hint::spin_loop();
        }
    }

    fn write_bytes(&self, _offset: u64, _buf: &[u8]) -> Result<usize> {
        // Writes still go through the legacy driver during the transition.
        // Will be implemented in a follow-up after reads are proven.
        Err(libcluu::Error::NotImplemented)
    }

    fn sector_size(&self) -> usize {
        self.sector_size_bytes
    }

    fn sector_count(&self) -> u64 {
        self.capacity_sectors
    }
}

/// Thread-safe wrapper for VirtioBlkDevice that implements BlockDevice.
///
/// Uses spin::Mutex for interior mutability since virtio operations need
/// mutable access internally, and BlockDevice requires Sync.
pub struct VirtioBlkAdapter {
    inner: Mutex<VirtioBlkDevice>,
}

impl VirtioBlkAdapter {
    /// Create a new adapter wrapping the given device.
    pub fn new(device: VirtioBlkDevice) -> Self {
        Self {
            inner: Mutex::new(device),
        }
    }
}

impl BlockDevice for VirtioBlkAdapter {
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let mut device = self.inner.lock();

        if buf.is_empty() {
            return Ok(0);
        }

        let sector_size = device.sector_size() as u64;
        let start_sector = offset / sector_size;
        let sector_offset = (offset % sector_size) as usize;

        // Calculate how many sectors we need
        let end_byte = offset + buf.len() as u64;
        let end_sector = end_byte.div_ceil(sector_size);
        let sector_count = end_sector - start_sector;

        // Read sectors
        let total_bytes = (sector_count as usize) * device.sector_size();
        let mut sector_buf = alloc::vec![0u8; total_bytes];

        device.read_sectors(start_sector, &mut sector_buf)?;

        // Copy the requested portion
        let copy_len = buf.len().min(sector_buf.len() - sector_offset);
        buf[..copy_len].copy_from_slice(&sector_buf[sector_offset..sector_offset + copy_len]);

        Ok(copy_len)
    }

    fn write_bytes(&self, offset: u64, buf: &[u8]) -> Result<usize> {
        let mut device = self.inner.lock();

        if buf.is_empty() {
            return Ok(0);
        }

        let sector_size = device.sector_size() as u64;
        let start_sector = offset / sector_size;
        let sector_offset = (offset % sector_size) as usize;

        // For partial sector writes, we need to read-modify-write
        let end_byte = offset + buf.len() as u64;
        let end_sector = end_byte.div_ceil(sector_size);
        let sector_count = end_sector - start_sector;

        let total_bytes = (sector_count as usize) * device.sector_size();
        let mut sector_buf = alloc::vec![0u8; total_bytes];

        // Read existing sectors if partial write
        if sector_offset != 0 || !buf.len().is_multiple_of(device.sector_size()) {
            device.read_sectors(start_sector, &mut sector_buf)?;
        }

        // Modify the data
        sector_buf[sector_offset..sector_offset + buf.len()].copy_from_slice(buf);

        // Write back
        device.write_sectors(start_sector, &sector_buf)?;

        Ok(buf.len())
    }

    fn sector_size(&self) -> usize {
        self.inner.lock().sector_size()
    }

    fn sector_count(&self) -> u64 {
        self.inner.lock().sector_count()
    }
}
