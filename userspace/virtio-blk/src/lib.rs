//! Virtio-blk block device driver for CLUU.
//!
//! This module provides the core virtio-blk implementation that implements
//! the `BlockDevice` trait from libcluu. Filesystem plugins (like ext2) can
//! use this trait directly without IPC overhead.

#![no_std]

extern crate alloc;

pub mod pci;
pub mod virtio;
pub mod virtqueue;

use libcluu::fs::BlockDevice;
use libcluu::Result;
use spin::Mutex;

pub use virtio::VirtioBlkDevice;

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
        let end_sector = (end_byte + sector_size - 1) / sector_size;
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
        let end_sector = (end_byte + sector_size - 1) / sector_size;
        let sector_count = end_sector - start_sector;

        let total_bytes = (sector_count as usize) * device.sector_size();
        let mut sector_buf = alloc::vec![0u8; total_bytes];

        // Read existing sectors if partial write
        if sector_offset != 0 || buf.len() % device.sector_size() != 0 {
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
