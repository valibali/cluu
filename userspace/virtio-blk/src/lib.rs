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

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use libcluu::fs::BlockDevice;
use libcluu::ipc::BLK_COMPLETE;
use libcluu::syscall::ipc_send;
use libcluu::types::Message;
use libcluu::Result;
use spin::Mutex;

pub use virtio::VirtioBlkDevice;

use crate::request_queue::BlkRequestQueue;
use crate::session::pack_cookie;
use cluu_virtio_core::transport::{ModernPciTransport, Transport};

/// Per-cookie bookkeeping for an asynchronous BLK_SUBMIT in flight. The
/// recv worker creates these on submit and consumes them when the device
/// completes the request via `drain_and_route`.
#[derive(Clone, Copy)]
pub struct PendingAsync {
    pub comp_ep: usize,
    pub rid: u64,
}

/// Driver-wide mutable state. Single-threaded today: the service main thread
/// holds this for the duration of each request (sync FS read or async
/// BLK_SUBMIT submission). When the main loop receives an IRQ message it
/// also calls `drain_and_route` to dispatch async `BLK_COMPLETE`s.
pub struct DriverStateInner {
    pub bq: BlkRequestQueue<ModernPciTransport>,
    /// Cookies belonging to async BLK_SUBMITs. Populated on submit; drained
    /// to a `BLK_COMPLETE` send by either the main loop's IRQ handler or
    /// the spin-poll inside `read_bytes`.
    pub pending: BTreeMap<u64, PendingAsync>,
}

pub struct DriverState {
    pub inner: Mutex<DriverStateInner>,
    /// Monotonic counter for the request-id half of sync FS cookies. Always
    /// packed with sid=0 via `session::pack_cookie`.
    pub next_sync_rid: AtomicU64,
}

impl DriverState {
    pub fn new(bq: BlkRequestQueue<ModernPciTransport>) -> Self {
        Self {
            inner: Mutex::new(DriverStateInner {
                bq,
                pending: BTreeMap::new(),
            }),
            next_sync_rid: AtomicU64::new(1),
        }
    }

    /// Allocate the next sync-path cookie, packed as (sid=0, rid).
    pub fn alloc_sync_cookie(&self) -> u64 {
        let rid = self.next_sync_rid.fetch_add(1, Ordering::Relaxed);
        pack_cookie(0, rid)
    }

    /// Drain the device used ring on IRQ wake, ack the device's ISR, and
    /// dispatch a `BLK_COMPLETE` for every async `BLK_SUBMIT` completion
    /// found. Sync FS cookies are silently dropped here — the spin-poll
    /// inside `read_bytes` is the canonical waiter and routes its own
    /// completion before this is reached.
    pub fn drain_and_route(&self) {
        let mut async_replies: Vec<(usize, u64, u8, u32)> = Vec::new();
        {
            let mut inner = self.inner.lock();
            let _ = inner.bq.transport.isr_status();
            let completions = inner.bq.drain_completions();
            for (cookie, status, blen) in completions {
                if let Some(p) = inner.pending.remove(&cookie) {
                    async_replies.push((p.comp_ep, p.rid, status, blen));
                }
            }
        }
        for (comp_ep, rid, status, blen) in async_replies {
            let msg = Message::new(
                BLK_COMPLETE,
                [rid as usize, status as usize, blen as usize, 0, 0, 0],
                3,
            );
            let _ = ipc_send(comp_ep, msg.as_bytes());
        }
    }
}

/// BlockDevice adapter built on the new virtio-core stack.
///
/// Submission goes through the shared `DriverState` so the recv worker can
/// route the device's completion back to the waiter via `sync_wake_endpoint`.
/// The pre-mapped scratch buffer at `scratch_base` provides aligned, pinned
/// physical pages for DMA without per-request `virt_to_phys` allocation.
pub struct ModernBlkAdapter {
    pub state: Arc<DriverState>,
    capacity_sectors: u64,
    sector_size_bytes: usize,
    scratch_base: usize,
    scratch_pages: usize,
    space_token: usize,
}

impl ModernBlkAdapter {
    pub fn new(
        state: Arc<DriverState>,
        capacity_sectors: u64,
        sector_size_bytes: usize,
        scratch_base: usize,
        scratch_pages: usize,
        space_token: usize,
    ) -> Self {
        Self {
            state,
            capacity_sectors,
            sector_size_bytes,
            scratch_base,
            scratch_pages,
            space_token,
        }
    }
}

impl BlockDevice for ModernBlkAdapter {
    fn read_bytes(&self, offset: u64, buf_out: &mut [u8]) -> Result<usize> {
        if buf_out.is_empty() {
            return Ok(0);
        }

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

        let n_pages = total_bytes.div_ceil(4096);
        let mut pages: alloc::vec::Vec<u64> = alloc::vec::Vec::with_capacity(n_pages);
        for i in 0..n_pages {
            let va = self.scratch_base + i * 4096;
            let phys = libcluu::syscall::virt_to_phys(self.space_token, va)? as u64;
            pages.push(phys);
        }

        // Spin-poll under the lock. Cluu's IRQ-via-IPC delivery has a
        // latent issue we can't pin down (causes wild-jump faults in
        // unrelated processes when ipc_recv is used to wait on the IRQ
        // endpoint from this driver's threads); the baseline works by
        // spin-polling and yielding, so we keep that pattern. While
        // polling we *also* route any async BLK_SUBMIT completions that
        // happen to arrive in the same drain — main thread will dispatch
        // their BLK_COMPLETE messages after we release the lock.
        let cookie = self.state.alloc_sync_cookie();
        let mut inner = self.state.inner.lock();
        inner
            .bq
            .submit_read(start_sector, &pages, total_bytes, cookie)?;
        inner.bq.notify();

        let mut deferred_async: Vec<(usize, u64, u8, u32)> = Vec::new();
        let mut my_status: Option<u8> = None;
        let mut spins = 0u64;
        while my_status.is_none() {
            let completions = inner.bq.drain_completions();
            for (got, status, blen) in completions {
                if got == cookie {
                    my_status = Some(status);
                } else if let Some(p) = inner.pending.remove(&got) {
                    deferred_async.push((p.comp_ep, p.rid, status, blen));
                }
                // Cookies neither ours nor in pending are silently dropped
                // (orphan completions from a closed session, etc.).
            }
            if my_status.is_some() {
                break;
            }
            spins += 1;
            if spins.is_multiple_of(1024) {
                let _ = libcluu::syscall::yield_cpu();
            }
            core::hint::spin_loop();
        }
        drop(inner);

        // Dispatch any async BLK_COMPLETEs that arrived while we polled.
        for (comp_ep, rid, status, blen) in deferred_async {
            let msg = Message::new(
                BLK_COMPLETE,
                [rid as usize, status as usize, blen as usize, 0, 0, 0],
                3,
            );
            let _ = ipc_send(comp_ep, msg.as_bytes());
        }

        if my_status.unwrap() != 0 {
            return Err(libcluu::Error::InvalidState);
        }
        let copy_len = buf_out.len().min(total_bytes - sector_offset);
        let scratch = unsafe {
            core::slice::from_raw_parts(self.scratch_base as *const u8, total_bytes)
        };
        buf_out[..copy_len].copy_from_slice(&scratch[sector_offset..sector_offset + copy_len]);
        Ok(copy_len)
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
