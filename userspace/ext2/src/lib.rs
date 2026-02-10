//! Ext2 filesystem plugin for CLUU.
//!
//! This library implements the `Filesystem` trait on top of a `BlockDevice`.
//! It follows the plugin pattern - the ext2 implementation can be plugged
//! into any block device service.

#![no_std]

extern crate alloc;

mod dir;
mod inode;
mod superblock;

pub use dir::{DirIter, RawDirEntry};
pub use inode::Inode;
pub use superblock::Superblock;

use alloc::string::String;
use alloc::vec::Vec;
use libcluu::fs::{BlockDevice, DirEntry, FileStat, Filesystem};
use libcluu::{Error, Result};

/// Ext2 filesystem instance.
///
/// This struct wraps a block device reference and provides ext2 filesystem
/// operations. It implements the `Filesystem` trait for use as a plugin.
pub struct Ext2Fs<'a> {
    block: &'a dyn BlockDevice,
    sb: Superblock,
    block_size: usize,
    inodes_per_group: u32,
    inode_size: usize,
}

impl<'a> Ext2Fs<'a> {
    /// Mount an ext2 filesystem from the given block device.
    pub fn mount(block: &'a dyn BlockDevice) -> Result<Self> {
        // Read superblock (always at byte offset 1024)
        let mut sb_buf = [0u8; 1024];
        block.read_bytes(1024, &mut sb_buf)?;

        let sb = Superblock::parse(&sb_buf)?;

        let block_size = 1024usize << sb.log_block_size;
        let inode_size = if sb.rev_level >= 1 {
            sb.inode_size as usize
        } else {
            128
        };
        let inodes_per_group = sb.inodes_per_group;

        Ok(Self {
            block,
            sb,
            block_size,
            inodes_per_group,
            inode_size,
        })
    }

    /// Read an inode by number.
    pub fn read_inode(&self, inode_num: u32) -> Result<Inode> {
        if inode_num == 0 || inode_num > self.sb.inodes_count {
            return Err(Error::InvalidArgument);
        }

        // Compute block group and index within group
        let group = (inode_num - 1) / self.inodes_per_group;
        let index = (inode_num - 1) % self.inodes_per_group;

        // Read the block group descriptor
        let bgd_block = if self.block_size == 1024 { 2 } else { 1 };
        let bgd_offset = bgd_block * self.block_size + (group as usize) * 32;

        let mut bgd_buf = [0u8; 32];
        self.block.read_bytes(bgd_offset as u64, &mut bgd_buf)?;

        let inode_table = u32::from_le_bytes([bgd_buf[8], bgd_buf[9], bgd_buf[10], bgd_buf[11]]);

        // Read the inode
        let inode_offset =
            (inode_table as usize) * self.block_size + (index as usize) * self.inode_size;

        let mut inode_buf = [0u8; 256]; // Max inode size
        self.block
            .read_bytes(inode_offset as u64, &mut inode_buf[..self.inode_size])?;

        Ok(Inode::parse(&inode_buf))
    }

    /// Look up a path component in a directory.
    fn lookup_in_dir(&self, dir_inode: &Inode, name: &str) -> Result<u32> {
        if !dir_inode.is_dir() {
            return Err(Error::InvalidArgument);
        }

        let dir_data = self.read_file_data(dir_inode)?;

        for entry in DirIter::new(&dir_data) {
            if entry.name == name {
                return Ok(entry.inode);
            }
        }

        Err(Error::NotFound)
    }

    /// Resolve a path to an inode number.
    pub fn resolve_path_to_inode(&self, path: &str) -> Result<u32> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Ok(2); // Root inode
        }

        let mut current_inode = 2u32;

        for component in path.split('/') {
            if component.is_empty() || component == "." {
                continue;
            }

            let inode = self.read_inode(current_inode)?;
            current_inode = self.lookup_in_dir(&inode, component)?;
        }

        Ok(current_inode)
    }

    /// Read file data into a buffer.
    pub fn read_file(&self, inode: &Inode, offset: usize, buf: &mut [u8]) -> Result<usize> {
        let file_size = inode.size() as usize;
        if offset >= file_size {
            return Ok(0);
        }

        let available = file_size - offset;
        let to_read = buf.len().min(available);

        // Read block by block
        let mut bytes_read = 0;
        let mut current_offset = offset;

        while bytes_read < to_read {
            let block_idx = current_offset / self.block_size;
            let block_offset = current_offset % self.block_size;
            let block_remaining = self.block_size - block_offset;
            let chunk_size = (to_read - bytes_read).min(block_remaining);

            let block_num = self.get_block_num(inode, block_idx as u32)?;

            if block_num == 0 {
                // Sparse block - fill with zeros
                buf[bytes_read..bytes_read + chunk_size].fill(0);
            } else {
                let block_byte_offset = (block_num as usize) * self.block_size + block_offset;
                self.block.read_bytes(
                    block_byte_offset as u64,
                    &mut buf[bytes_read..bytes_read + chunk_size],
                )?;
            }

            bytes_read += chunk_size;
            current_offset += chunk_size;
        }

        Ok(bytes_read)
    }

    /// Read all file data (for small files like directories).
    fn read_file_data(&self, inode: &Inode) -> Result<Vec<u8>> {
        let size = inode.size() as usize;
        let mut data = alloc::vec![0u8; size];
        self.read_file(inode, 0, &mut data)?;
        Ok(data)
    }

    /// Get the block number for a logical block index.
    fn get_block_num(&self, inode: &Inode, block_idx: u32) -> Result<u32> {
        let ptrs_per_block = self.block_size / 4;

        if block_idx < 12 {
            // Direct block
            Ok(inode.direct_blocks[block_idx as usize])
        } else if block_idx < 12 + ptrs_per_block as u32 {
            // Indirect block
            let idx = block_idx - 12;
            self.read_indirect_block(inode.indirect_block, idx as usize)
        } else if block_idx < 12 + ptrs_per_block as u32 + (ptrs_per_block * ptrs_per_block) as u32
        {
            // Double indirect
            let idx = block_idx - 12 - ptrs_per_block as u32;
            let i1 = idx / ptrs_per_block as u32;
            let i2 = idx % ptrs_per_block as u32;

            let indirect1 = self.read_indirect_block(inode.double_indirect, i1 as usize)?;
            if indirect1 == 0 {
                return Ok(0);
            }
            self.read_indirect_block(indirect1, i2 as usize)
        } else {
            // Triple indirect - not implemented for simplicity
            Err(Error::InvalidOperation)
        }
    }

    /// Read a single block pointer from an indirect block.
    fn read_indirect_block(&self, block_num: u32, index: usize) -> Result<u32> {
        if block_num == 0 {
            return Ok(0);
        }

        let offset = (block_num as usize) * self.block_size + index * 4;
        let mut buf = [0u8; 4];
        self.block.read_bytes(offset as u64, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    /// List directory entries (internal helper).
    fn readdir_internal(&self, inode: &Inode) -> Result<Vec<dir::RawDirEntry>> {
        if !inode.is_dir() {
            return Err(Error::InvalidArgument);
        }

        let data = self.read_file_data(inode)?;
        Ok(DirIter::new(&data).collect())
    }

    /// Get filesystem block size.
    pub fn block_size(&self) -> usize {
        self.block_size
    }
}

/// Filesystem trait implementation for ext2.
///
/// This allows ext2 to be used as a plugin in the block device service.
impl<'a> Filesystem for Ext2Fs<'a> {
    fn name(&self) -> &'static str {
        "ext2"
    }

    fn root_inode(&self) -> u64 {
        2 // Ext2 root is always inode 2
    }

    fn lookup(&self, dir_inode: u64, name: &str) -> Result<u64> {
        let inode = self.read_inode(dir_inode as u32)?;
        self.lookup_in_dir(&inode, name).map(|n| n as u64)
    }

    fn stat(&self, inode: u64) -> Result<FileStat> {
        let ino = self.read_inode(inode as u32)?;
        Ok(FileStat {
            inode,
            size: ino.size(),
            is_dir: ino.is_dir(),
            is_file: ino.is_file(),
        })
    }

    fn readdir(&self, inode: u64) -> Result<Vec<DirEntry>> {
        let ino = self.read_inode(inode as u32)?;
        let raw_entries = self.readdir_internal(&ino)?;

        Ok(raw_entries
            .into_iter()
            .map(|e| DirEntry {
                name: String::from(e.name),
                inode: e.inode as u64,
                is_dir: {
                    // Check if entry is a directory by reading its inode
                    self.read_inode(e.inode)
                        .map(|i| i.is_dir())
                        .unwrap_or(false)
                },
            })
            .collect())
    }

    fn read(&self, inode: u64, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let ino = self.read_inode(inode as u32)?;
        self.read_file(&ino, offset as usize, buf)
    }
}
