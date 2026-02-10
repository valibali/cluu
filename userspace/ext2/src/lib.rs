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

    /// Compute block-group index and inode-table byte offset for an inode.
    fn inode_disk_offset(&self, inode_num: u32) -> Result<(u32, usize)> {
        if inode_num == 0 || inode_num > self.sb.inodes_count {
            return Err(Error::InvalidArgument);
        }

        let group = (inode_num - 1) / self.inodes_per_group;
        let index = (inode_num - 1) % self.inodes_per_group;

        let bgd = self.read_group_desc(group)?;
        let inode_table = u32::from_le_bytes([bgd[8], bgd[9], bgd[10], bgd[11]]);
        let inode_offset =
            (inode_table as usize) * self.block_size + (index as usize) * self.inode_size;
        Ok((group, inode_offset))
    }

    /// Persist inode metadata to disk.
    fn write_inode(&self, inode_num: u32, inode: &Inode) -> Result<()> {
        let (_, inode_offset) = self.inode_disk_offset(inode_num)?;
        let mut inode_buf = [0u8; 256];
        self.block
            .read_bytes(inode_offset as u64, &mut inode_buf[..self.inode_size])?;
        inode.write_to(&mut inode_buf[..self.inode_size]);
        self.block
            .write_bytes(inode_offset as u64, &inode_buf[..self.inode_size])?;
        Ok(())
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

    /// Write file data from a buffer, allocating new blocks as needed.
    pub fn write_file_by_num(&self, inode_num: u32, offset: usize, buf: &[u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut inode = self.read_inode(inode_num)?;
        if !inode.is_file() {
            return Err(Error::InvalidOperation);
        }

        let old_size = inode.size();
        let end = offset.checked_add(buf.len()).ok_or(Error::Overflow)?;
        let mut bytes_written = 0;
        let mut current_offset = offset;

        while bytes_written < buf.len() {
            let block_idx = current_offset / self.block_size;
            let block_offset = current_offset % self.block_size;
            let block_remaining = self.block_size - block_offset;
            let chunk_size = (buf.len() - bytes_written).min(block_remaining);

            let block_num = self.get_or_alloc_block_num(&mut inode, block_idx as u32)?;
            let block_byte_offset = (block_num as usize) * self.block_size;
            if block_offset == 0 && chunk_size == self.block_size {
                self.block.write_bytes(
                    block_byte_offset as u64,
                    &buf[bytes_written..bytes_written + chunk_size],
                )?;
            } else {
                let mut block_buf = alloc::vec![0u8; self.block_size];
                self.block
                    .read_bytes(block_byte_offset as u64, &mut block_buf)?;
                block_buf[block_offset..block_offset + chunk_size]
                    .copy_from_slice(&buf[bytes_written..bytes_written + chunk_size]);
                self.block
                    .write_bytes(block_byte_offset as u64, &block_buf)?;
            }

            bytes_written += chunk_size;
            current_offset += chunk_size;
        }

        let end_u64 = end as u64;
        if end_u64 > old_size {
            inode.set_size(end_u64);
        }
        self.write_inode(inode_num, &inode)?;
        Ok(bytes_written)
    }

    /// Compatibility wrapper retained for callers that already hold an inode.
    pub fn write_file(&self, inode: &Inode, offset: usize, buf: &[u8]) -> Result<usize> {
        if !inode.is_file() {
            return Err(Error::InvalidOperation);
        }

        let file_size = inode.size() as usize;
        if offset >= file_size {
            return Ok(0);
        }

        let available = file_size - offset;
        let to_write = buf.len().min(available);

        let mut bytes_written = 0;
        let mut current_offset = offset;
        while bytes_written < to_write {
            let block_idx = current_offset / self.block_size;
            let block_offset = current_offset % self.block_size;
            let block_remaining = self.block_size - block_offset;
            let chunk_size = (to_write - bytes_written).min(block_remaining);

            let block_num = self.get_block_num(inode, block_idx as u32)?;
            if block_num == 0 {
                return Err(Error::InvalidOperation);
            }

            let block_byte_offset = (block_num as usize) * self.block_size;
            if block_offset == 0 && chunk_size == self.block_size {
                self.block.write_bytes(
                    block_byte_offset as u64,
                    &buf[bytes_written..bytes_written + chunk_size],
                )?;
            } else {
                let mut block_buf = alloc::vec![0u8; self.block_size];
                self.block
                    .read_bytes(block_byte_offset as u64, &mut block_buf)?;
                block_buf[block_offset..block_offset + chunk_size]
                    .copy_from_slice(&buf[bytes_written..bytes_written + chunk_size]);
                self.block
                    .write_bytes(block_byte_offset as u64, &block_buf)?;
            }

            bytes_written += chunk_size;
            current_offset += chunk_size;
        }

        Ok(bytes_written)
    }

    /// Write to inode by inode number.
    pub fn write_by_inode(&self, inode: u64, offset: u64, data: &[u8]) -> Result<usize> {
        self.write_file_by_num(inode as u32, offset as usize, data)
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

    /// Resolve (and allocate if needed) a logical block to a physical block number.
    fn get_or_alloc_block_num(&self, inode: &mut Inode, block_idx: u32) -> Result<u32> {
        let ptrs_per_block = (self.block_size / 4) as u32;
        if block_idx < 12 {
            let slot = &mut inode.direct_blocks[block_idx as usize];
            if *slot == 0 {
                *slot = self.allocate_block()?;
                inode.blocks = inode.blocks.saturating_add((self.block_size / 512) as u32);
            }
            return Ok(*slot);
        }

        if block_idx < 12 + ptrs_per_block {
            let idx = (block_idx - 12) as usize;
            if inode.indirect_block == 0 {
                inode.indirect_block = self.allocate_block()?;
                inode.blocks = inode.blocks.saturating_add((self.block_size / 512) as u32);
                self.zero_block(inode.indirect_block)?;
            }
            return self.get_or_alloc_indirect_ptr(inode, idx);
        }

        if block_idx < 12 + ptrs_per_block + ptrs_per_block * ptrs_per_block {
            let rel = block_idx - 12 - ptrs_per_block;
            let i1 = (rel / ptrs_per_block) as usize;
            let i2 = (rel % ptrs_per_block) as usize;
            if inode.double_indirect == 0 {
                inode.double_indirect = self.allocate_block()?;
                inode.blocks = inode.blocks.saturating_add((self.block_size / 512) as u32);
                self.zero_block(inode.double_indirect)?;
            }

            let mut lvl1 = self.read_indirect_block(inode.double_indirect, i1)?;
            if lvl1 == 0 {
                lvl1 = self.allocate_block()?;
                inode.blocks = inode.blocks.saturating_add((self.block_size / 512) as u32);
                self.zero_block(lvl1)?;
                self.write_indirect_block(inode.double_indirect, i1, lvl1)?;
            }

            let mut data = self.read_indirect_block(lvl1, i2)?;
            if data == 0 {
                data = self.allocate_block()?;
                inode.blocks = inode.blocks.saturating_add((self.block_size / 512) as u32);
                self.write_indirect_block(lvl1, i2, data)?;
            }
            return Ok(data);
        }

        Err(Error::InvalidOperation)
    }

    fn get_or_alloc_indirect_ptr(&self, inode: &mut Inode, idx: usize) -> Result<u32> {
        let mut block = self.read_indirect_block(inode.indirect_block, idx)?;
        if block == 0 {
            block = self.allocate_block()?;
            inode.blocks = inode.blocks.saturating_add((self.block_size / 512) as u32);
            self.write_indirect_block(inode.indirect_block, idx, block)?;
        }
        Ok(block)
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

    fn write_indirect_block(&self, block_num: u32, index: usize, value: u32) -> Result<()> {
        if block_num == 0 {
            return Err(Error::InvalidArgument);
        }
        let offset = (block_num as usize) * self.block_size + index * 4;
        self.block
            .write_bytes(offset as u64, &value.to_le_bytes())?;
        Ok(())
    }

    fn zero_block(&self, block_num: u32) -> Result<()> {
        let zero = alloc::vec![0u8; self.block_size];
        let off = (block_num as usize) * self.block_size;
        self.block.write_bytes(off as u64, &zero)?;
        Ok(())
    }

    fn group_desc_offset(&self, group: u32) -> usize {
        let bgd_block = if self.block_size == 1024 { 2 } else { 1 };
        bgd_block * self.block_size + (group as usize) * 32
    }

    fn read_group_desc(&self, group: u32) -> Result<[u8; 32]> {
        let mut buf = [0u8; 32];
        self.block
            .read_bytes(self.group_desc_offset(group) as u64, &mut buf)?;
        Ok(buf)
    }

    fn write_group_desc(&self, group: u32, desc: &[u8; 32]) -> Result<()> {
        self.block
            .write_bytes(self.group_desc_offset(group) as u64, desc)?;
        Ok(())
    }

    /// Allocate a free data block using ext2 block bitmap metadata.
    fn allocate_block(&self) -> Result<u32> {
        let groups = self.sb.blocks_count.div_ceil(self.sb.blocks_per_group);
        for group in 0..groups {
            let mut desc = self.read_group_desc(group)?;
            let free_blocks = u16::from_le_bytes([desc[12], desc[13]]);
            if free_blocks == 0 {
                continue;
            }

            let bitmap_block = u32::from_le_bytes([desc[0], desc[1], desc[2], desc[3]]);
            let bitmap_offset = (bitmap_block as usize) * self.block_size;
            let mut bitmap = alloc::vec![0u8; self.block_size];
            self.block.read_bytes(bitmap_offset as u64, &mut bitmap)?;

            for bit_idx in 0..self.sb.blocks_per_group as usize {
                let byte = bit_idx / 8;
                let mask = 1u8 << (bit_idx % 8);
                if byte >= bitmap.len() {
                    break;
                }
                if bitmap[byte] & mask != 0 {
                    continue;
                }

                let block_num =
                    self.sb.first_data_block + group * self.sb.blocks_per_group + bit_idx as u32;
                if block_num >= self.sb.blocks_count {
                    break;
                }

                bitmap[byte] |= mask;
                self.block.write_bytes(bitmap_offset as u64, &bitmap)?;
                self.zero_block(block_num)?;

                let new_group_free = free_blocks.saturating_sub(1);
                desc[12..14].copy_from_slice(&new_group_free.to_le_bytes());
                self.write_group_desc(group, &desc)?;
                self.dec_superblock_free_blocks()?;
                return Ok(block_num);
            }
        }
        Err(Error::OutOfMemory)
    }

    fn dec_superblock_free_blocks(&self) -> Result<()> {
        let mut sb_buf = [0u8; 1024];
        self.block.read_bytes(1024, &mut sb_buf)?;
        let cur = u32::from_le_bytes([sb_buf[12], sb_buf[13], sb_buf[14], sb_buf[15]]);
        let next = cur.saturating_sub(1);
        sb_buf[12..16].copy_from_slice(&next.to_le_bytes());
        self.block.write_bytes(1024, &sb_buf)?;
        Ok(())
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
