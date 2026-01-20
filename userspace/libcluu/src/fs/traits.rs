//! Filesystem and block device traits for plugin architecture.
//!
//! These traits enable SOLID principles:
//! - Dependency Inversion: High-level modules depend on abstractions
//! - Open/Closed: New filesystems can be added without modifying existing code
//! - Liskov Substitution: Any Filesystem impl can be used interchangeably

use alloc::string::String;
use alloc::vec::Vec;
use crate::error::Result;

/// Block device trait for low-level storage access.
///
/// Implementors provide raw sector-level read/write operations.
pub trait BlockDevice: Send + Sync {
    /// Read bytes from the device at the given byte offset.
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<usize>;

    /// Write bytes to the device at the given byte offset.
    fn write_bytes(&self, offset: u64, buf: &[u8]) -> Result<usize>;

    /// Get the sector size in bytes.
    fn sector_size(&self) -> usize;

    /// Get the total number of sectors.
    fn sector_count(&self) -> u64;

    /// Get total device size in bytes.
    fn size(&self) -> u64 {
        self.sector_count() * self.sector_size() as u64
    }
}

/// File metadata returned by stat operations.
#[derive(Debug, Clone)]
pub struct FileStat {
    /// Inode number or unique identifier.
    pub inode: u64,
    /// File size in bytes.
    pub size: u64,
    /// True if this is a directory.
    pub is_dir: bool,
    /// True if this is a regular file.
    pub is_file: bool,
}

/// Directory entry returned by readdir operations.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Entry name.
    pub name: String,
    /// Inode number or unique identifier.
    pub inode: u64,
    /// True if this entry is a directory.
    pub is_dir: bool,
}

/// Filesystem plugin trait.
///
/// Implementors provide filesystem operations on top of a block device.
/// This follows the plugin pattern - different filesystem types (ext2, fat32, etc.)
/// can implement this trait and be plugged into a block device service.
pub trait Filesystem: Send + Sync {
    /// Get filesystem name (e.g., "ext2", "fat32").
    fn name(&self) -> &'static str;

    /// Get the root directory inode/identifier.
    fn root_inode(&self) -> u64;

    /// Look up a name in a directory, returning the inode of the entry.
    fn lookup(&self, dir_inode: u64, name: &str) -> Result<u64>;

    /// Get file/directory metadata.
    fn stat(&self, inode: u64) -> Result<FileStat>;

    /// Read directory entries.
    fn readdir(&self, inode: u64) -> Result<Vec<DirEntry>>;

    /// Read file data.
    fn read(&self, inode: u64, offset: u64, buf: &mut [u8]) -> Result<usize>;

    /// Resolve a path to an inode.
    fn resolve_path(&self, path: &str) -> Result<u64> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Ok(self.root_inode());
        }

        let mut current = self.root_inode();

        for component in path.split('/') {
            if component.is_empty() || component == "." {
                continue;
            }
            current = self.lookup(current, component)?;
        }

        Ok(current)
    }
}
