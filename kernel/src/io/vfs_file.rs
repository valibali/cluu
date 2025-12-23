/*
 * VFS File Device
 *
 * Device implementation for files opened through the VFS server.
 * Provides zero-copy reads by accessing the fsitem directly in shared memory.
 */

use super::device::{Device, Errno, Stat, S_IFREG};
use crate::shmem::ShmemId;
use x86_64::VirtAddr;

/// VFS file device
///
/// Represents an open file from the VFS server. The file data is stored
/// in a shared memory fsitem structure, allowing zero-copy reads.
pub struct VfsFile {
    vfs_fd: i32,           // VFS server's file descriptor
    shmem_id: ShmemId,     // Shared memory ID for fsitem
    fsitem_addr: VirtAddr, // Virtual address of mapped fsitem
    offset: core::sync::atomic::AtomicU64, // Current read/write offset
}

impl VfsFile {
    /// Create a new VFS file device
    ///
    /// # Arguments
    /// * `vfs_fd` - File descriptor from VFS server
    /// * `shmem_id` - Shared memory ID containing the fsitem
    /// * `fsitem_addr` - Virtual address where fsitem is mapped
    pub fn new(vfs_fd: i32, shmem_id: ShmemId, fsitem_addr: VirtAddr) -> Self {
        Self {
            vfs_fd,
            shmem_id,
            fsitem_addr,
            offset: core::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Get VFS file descriptor
    pub fn vfs_fd(&self) -> i32 {
        self.vfs_fd
    }

    /// Read fsitem header from shared memory
    fn read_fsitem_header(&self) -> Result<FsItemHeader, Errno> {
        // Read fsitem structure from mapped memory
        let ptr = self.fsitem_addr.as_u64() as *const FsItemHeader;

        // Safety: fsitem_addr points to a valid mapped fsitem in shared memory
        // Using read_unaligned to avoid alignment issues with packed struct
        let header = unsafe { core::ptr::read_unaligned(ptr) };

        // Validate magic number (copy to local to avoid packed struct reference)
        let magic = header.magic;
        if magic != 0x46534954 {
            log::error!("VfsFile: invalid fsitem magic: 0x{:x}", magic);
            return Err(Errno::EIO);
        }

        Ok(header)
    }
}

/// Fsitem header structure (matches userspace/lib/fsitem.h)
#[repr(C, packed)]
#[derive(Copy, Clone)]
struct FsItemHeader {
    magic: u32,         // 0x46534954 ("FSIT")
    version: u32,       // 1
    type_: u32,         // FSITEM_TYPE_FILE
    flags: u32,         // Open flags
    size: u64,          // File size in bytes
    fs_type: u32,       // Filesystem type
    mode: u32,          // Unix mode
    data_offset: u64,   // Offset to file data (4096)
    offset: u64,        // Current position (unused, we track in device)
    ref_count: u32,     // Reference count
    lock: u32,          // Spinlock
    // path: [u8; 256] follows but we don't need it for reads
}

impl Device for VfsFile {
    fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Read fsitem header to get file size and data offset
        let header = self.read_fsitem_header()?;

        // Copy header fields to locals to avoid packed struct alignment issues
        let file_size = header.size;
        let data_offset = header.data_offset;

        // Get current offset
        let current_offset = self.offset.load(core::sync::atomic::Ordering::SeqCst);

        // Check if at EOF
        if current_offset >= file_size {
            return Ok(0); // EOF
        }

        // Calculate how many bytes to read
        let remaining = (file_size - current_offset) as usize;
        let to_read = remaining.min(buf.len());

        // Calculate address of file data
        let data_addr = self.fsitem_addr.as_u64() + data_offset + current_offset;
        let data_ptr = data_addr as *const u8;

        // Copy data from fsitem to user buffer (zero-copy from VFS perspective!)
        unsafe {
            core::ptr::copy_nonoverlapping(data_ptr, buf.as_mut_ptr(), to_read);
        }

        // Update offset
        self.offset.fetch_add(to_read as u64, core::sync::atomic::Ordering::SeqCst);

        log::debug!("VfsFile::read: read {} bytes at offset {}", to_read, current_offset);
        Ok(to_read)
    }

    fn write(&self, _buf: &[u8]) -> Result<usize, Errno> {
        // VFS files are currently read-only
        Err(Errno::EACCES)
    }

    fn ioctl(&self, _request: u32, _arg: usize) -> Result<i32, Errno> {
        Err(Errno::ENOTTY)
    }

    fn is_tty(&self) -> bool {
        false
    }

    fn stat(&self) -> Stat {
        // Try to read fsitem header for accurate size
        let size = self.read_fsitem_header()
            .map(|h| {
                // Copy to local to avoid packed struct reference
                let s = h.size;
                s
            })
            .unwrap_or(0);

        Stat {
            st_mode: S_IFREG | 0o644, // Regular file, rw-r--r--
            st_size: size,
            st_blksize: 4096,
            st_blocks: ((size + 4095) / 4096) as u64,
        }
    }

    fn seek(&self, offset: i64, whence: i32) -> Result<i64, Errno> {
        const SEEK_SET: i32 = 0;
        const SEEK_CUR: i32 = 1;
        const SEEK_END: i32 = 2;

        // Read file size from fsitem
        let header = self.read_fsitem_header()?;
        // Copy to local to avoid packed struct reference
        let file_size = header.size as i64;

        // Get current offset
        let current_offset = self.offset.load(core::sync::atomic::Ordering::SeqCst) as i64;

        // Calculate new offset
        let new_offset = match whence {
            SEEK_SET => offset,
            SEEK_CUR => current_offset + offset,
            SEEK_END => file_size + offset,
            _ => return Err(Errno::EINVAL),
        };

        // Validate new offset
        if new_offset < 0 {
            return Err(Errno::EINVAL);
        }

        // Update offset
        self.offset.store(new_offset as u64, core::sync::atomic::Ordering::SeqCst);

        Ok(new_offset)
    }
}
