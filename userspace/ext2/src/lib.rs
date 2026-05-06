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
use spin::Mutex;

/// One-slot cache for indirect block contents.
///
/// Sequential file reads call `read_indirect_block(block_num, idx)` once per
/// data block, asking for one 4-byte pointer at a time. Without caching, every
/// call issues a fresh DMA round-trip to the same physical block (just to read
/// 4 bytes from it). For a 4 MB file that's ~1024 redundant 4-byte DMAs.
///
/// One slot is enough for sequential reads on the boot path: each indirect
/// block covers 1024 data blocks (= 4 MB at block_size=4 KiB), and the 4 MB
/// ELF binaries each fit comfortably in one indirect block.
struct IndirectCache {
    block_num: u32,
    contents: Vec<u8>,
}

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
    /// Cached indirect block — one slot, replaced on every miss. Mutex because
    /// `read_indirect_block` takes `&self`. Contents is empty when the cache
    /// is invalid.
    indirect_cache: Mutex<IndirectCache>,
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
            indirect_cache: Mutex::new(IndirectCache {
                block_num: 0,
                contents: Vec::new(),
            }),
        })
    }

    /// Invalidate the indirect-block cache. Called from any path that mutates
    /// indirect blocks on disk (write_indirect_block, allocate_block, etc.)
    /// so a subsequent read sees the updated content.
    fn invalidate_indirect_cache(&self) {
        let mut cache = self.indirect_cache.lock();
        cache.block_num = 0;
        cache.contents.clear();
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
    ///
    /// Sequential-read fast path: walks the file's logical block numbers and
    /// detects runs of physically-contiguous blocks (the common case for
    /// freshly-written ELF binaries). Each contiguous run is served by a
    /// single underlying `read_bytes` call — taking what would otherwise be
    /// hundreds of 4 KiB DMAs and collapsing them into a handful of large
    /// ones.  Sparse holes inside a run are also handled in one shot via
    /// `fill(0)`.
    pub fn read_file(&self, inode: &Inode, offset: usize, buf: &mut [u8]) -> Result<usize> {
        let file_size = inode.size() as usize;
        if offset >= file_size {
            return Ok(0);
        }
        let available = file_size - offset;
        let to_read = buf.len().min(available);
        if to_read == 0 {
            return Ok(0);
        }

        let mut bytes_read = 0usize;
        let mut current_offset = offset;

        while bytes_read < to_read {
            // Position inside the current logical block.
            let block_idx = (current_offset / self.block_size) as u32;
            let block_offset = current_offset % self.block_size;
            let first_block_remaining = self.block_size - block_offset;

            // Look up the first block's physical number.
            let first_block_num = self.get_block_num(inode, block_idx)?;

            // Build a run of consecutive logical blocks that are *also*
            // physically contiguous (or all-sparse). Cap the run at the
            // bytes still requested.
            let mut run_logical_blocks = 1u32;
            let max_remaining = to_read - bytes_read;
            // First chunk only takes block_offset..block_size of its block;
            // every subsequent block contributes a full block until we hit
            // the user's request limit or a discontinuity.
            let mut run_bytes = first_block_remaining.min(max_remaining);
            while run_bytes < max_remaining {
                let next_logical = block_idx + run_logical_blocks;
                let next_phys = self.get_block_num(inode, next_logical)?;
                let extends = if first_block_num == 0 {
                    // Sparse run: keep extending while next block is also sparse.
                    next_phys == 0
                } else {
                    // Allocated run: keep extending while the next block
                    // sits immediately after the previous (i.e. no fragmentation).
                    next_phys != 0
                        && next_phys == first_block_num + run_logical_blocks
                };
                if !extends {
                    break;
                }
                run_logical_blocks += 1;
                let take = self.block_size.min(max_remaining - run_bytes);
                run_bytes += take;
                if take < self.block_size {
                    break; // Reached user's byte limit mid-block.
                }
            }

            let dst = &mut buf[bytes_read..bytes_read + run_bytes];
            if first_block_num == 0 {
                dst.fill(0);
            } else {
                let phys_byte_offset =
                    (first_block_num as usize) * self.block_size + block_offset;
                self.block.read_bytes(phys_byte_offset as u64, dst)?;
            }

            bytes_read += run_bytes;
            current_offset += run_bytes;
        }

        Ok(bytes_read)
    }

    /// Write file data from a buffer, allocating new blocks as needed.
    pub fn write_file_by_num(&self, inode_num: u32, offset: usize, buf: &[u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut inode = self.read_inode(inode_num)?;
        if !inode.is_file() && !inode.is_dir() {
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

    pub fn unlink_path(&self, path: &str) -> Result<()> {
        let (parent_ino, name) = self.resolve_parent(path)?;
        let (entry, mut parent_data) = self.find_dir_entry(parent_ino, name)?;
        let target = self.read_inode(entry.inode)?;
        if target.is_dir() {
            return Err(Error::InvalidOperation);
        }

        self.clear_dir_entry(&mut parent_data, entry.offset);
        self.write_file_by_num(parent_ino, 0, &parent_data)?;

        let mut target_inode = target;
        if target_inode.links_count > 0 {
            target_inode.links_count -= 1;
            self.write_inode(entry.inode, &target_inode)?;
        }
        Ok(())
    }

    pub fn link_path(&self, old_path: &str, new_path: &str) -> Result<()> {
        let old_ino = self.resolve_path_to_inode(old_path)?;
        let old_inode = self.read_inode(old_ino)?;
        if old_inode.is_dir() {
            return Err(Error::InvalidOperation);
        }
        if self.resolve_path_to_inode(new_path).is_ok() {
            return Err(Error::AlreadyExists);
        }
        let (new_parent_ino, new_name) = self.resolve_parent(new_path)?;
        self.add_dir_entry(new_parent_ino, new_name, old_ino, dir::FT_REG_FILE)?;
        let mut inode = old_inode;
        inode.links_count = inode.links_count.saturating_add(1);
        self.write_inode(old_ino, &inode)?;
        Ok(())
    }

    pub fn rmdir_path(&self, path: &str) -> Result<()> {
        let (parent_ino, name) = self.resolve_parent(path)?;
        let (entry, mut parent_data) = self.find_dir_entry(parent_ino, name)?;
        if entry.inode == 2 {
            return Err(Error::InvalidOperation);
        }
        let target = self.read_inode(entry.inode)?;
        if !target.is_dir() {
            return Err(Error::InvalidOperation);
        }
        let child_entries = self.parse_dir_entries(&self.read_file_data(&target)?);
        for e in &child_entries {
            if e.inode != 0 && e.name != "." && e.name != ".." {
                return Err(Error::Busy);
            }
        }

        self.clear_dir_entry(&mut parent_data, entry.offset);
        self.write_file_by_num(parent_ino, 0, &parent_data)?;

        let mut parent = self.read_inode(parent_ino)?;
        if parent.links_count > 0 {
            parent.links_count -= 1;
            self.write_inode(parent_ino, &parent)?;
        }

        let mut child = target;
        child.links_count = 0;
        self.write_inode(entry.inode, &child)?;
        Ok(())
    }

    pub fn mkdir_path(&self, path: &str, mode: u16) -> Result<()> {
        if self.resolve_path_to_inode(path).is_ok() {
            return Err(Error::AlreadyExists);
        }
        let (parent_ino, name) = self.resolve_parent(path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if !parent.is_dir() {
            return Err(Error::InvalidOperation);
        }

        let new_inode_num = self.allocate_inode()?;
        let data_block = self.allocate_block()?;
        let mut new_inode = Inode {
            mode: (inode::S_IFDIR | (mode & 0o777)),
            uid: 0,
            size_lo: self.block_size as u32,
            atime: 0,
            ctime: 0,
            mtime: 0,
            dtime: 0,
            gid: 0,
            links_count: 2,
            blocks: (self.block_size / 512) as u32,
            flags: 0,
            direct_blocks: [0; 12],
            indirect_block: 0,
            double_indirect: 0,
            triple_indirect: 0,
            size_hi: 0,
        };
        new_inode.direct_blocks[0] = data_block;
        self.write_inode(new_inode_num, &new_inode)?;

        let mut block = alloc::vec![0u8; self.block_size];
        self.write_dir_record(&mut block, 0, new_inode_num, 12, ".", dir::FT_DIR)?;
        self.write_dir_record(
            &mut block,
            12,
            parent_ino,
            (self.block_size - 12) as u16,
            "..",
            dir::FT_DIR,
        )?;
        let block_off = (data_block as usize) * self.block_size;
        self.block.write_bytes(block_off as u64, &block)?;

        self.add_dir_entry(parent_ino, name, new_inode_num, dir::FT_DIR)?;
        parent.links_count = parent.links_count.saturating_add(1);
        self.write_inode(parent_ino, &parent)?;
        Ok(())
    }

    pub fn rename_path(&self, old_path: &str, new_path: &str) -> Result<()> {
        if old_path == new_path {
            return Ok(());
        }

        let (old_parent_ino, old_name) = self.resolve_parent(old_path)?;
        let (new_parent_ino, new_name) = self.resolve_parent(new_path)?;
        if self.lookup_entry(new_parent_ino, new_name)?.is_some() {
            return Err(Error::AlreadyExists);
        }

        let (old_entry, mut old_parent_data) = self.find_dir_entry(old_parent_ino, old_name)?;
        let old_inode = self.read_inode(old_entry.inode)?;
        let file_type = if old_inode.is_dir() {
            dir::FT_DIR
        } else {
            dir::FT_REG_FILE
        };

        if old_parent_ino == new_parent_ino
            && self.can_rename_in_place(&old_parent_data, old_entry.offset, new_name)
        {
            self.update_dir_entry_name(&mut old_parent_data, old_entry.offset, new_name)?;
            self.write_file_by_num(old_parent_ino, 0, &old_parent_data)?;
            return Ok(());
        }

        if old_inode.is_dir() && old_parent_ino != new_parent_ino {
            return Err(Error::NotImplemented);
        }

        self.add_dir_entry(new_parent_ino, new_name, old_entry.inode, file_type)?;
        self.clear_dir_entry(&mut old_parent_data, old_entry.offset);
        self.write_file_by_num(old_parent_ino, 0, &old_parent_data)?;
        Ok(())
    }

    pub fn create_file_path(&self, path: &str, mode: u16) -> Result<u32> {
        if self.resolve_path_to_inode(path).is_ok() {
            return Err(Error::AlreadyExists);
        }
        let (parent_ino, name) = self.resolve_parent(path)?;
        let parent = self.read_inode(parent_ino)?;
        if !parent.is_dir() {
            return Err(Error::InvalidOperation);
        }

        let inode_num = self.allocate_inode()?;
        let inode = Inode {
            mode: (inode::S_IFREG | (mode & 0o777)),
            uid: 0,
            size_lo: 0,
            atime: 0,
            ctime: 0,
            mtime: 0,
            dtime: 0,
            gid: 0,
            links_count: 1,
            blocks: 0,
            flags: 0,
            direct_blocks: [0; 12],
            indirect_block: 0,
            double_indirect: 0,
            triple_indirect: 0,
            size_hi: 0,
        };
        self.write_inode(inode_num, &inode)?;
        self.add_dir_entry(parent_ino, name, inode_num, dir::FT_REG_FILE)?;
        Ok(inode_num)
    }

    /// Read all file data (for small files like directories).
    fn read_file_data(&self, inode: &Inode) -> Result<Vec<u8>> {
        let size = inode.size() as usize;
        let mut data = alloc::vec![0u8; size];
        self.read_file(inode, 0, &mut data)?;
        Ok(data)
    }

    fn resolve_parent<'b>(&self, path: &'b str) -> Result<(u32, &'b str)> {
        let norm = path.trim_matches('/');
        if norm.is_empty() {
            return Err(Error::InvalidArgument);
        }
        let (parent, name) = if let Some(pos) = norm.rfind('/') {
            (&norm[..pos], &norm[pos + 1..])
        } else {
            ("", norm)
        };
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(Error::InvalidArgument);
        }
        let parent_inode = if parent.is_empty() {
            2
        } else {
            self.resolve_path_to_inode(parent)?
        };
        Ok((parent_inode, name))
    }

    fn lookup_entry(&self, dir_inode_num: u32, name: &str) -> Result<Option<DirEntryMeta>> {
        let dir_inode = self.read_inode(dir_inode_num)?;
        if !dir_inode.is_dir() {
            return Err(Error::InvalidOperation);
        }
        let data = self.read_file_data(&dir_inode)?;
        for entry in self.parse_dir_entries(&data) {
            if entry.inode != 0 && entry.name == name {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    fn find_dir_entry(&self, dir_inode_num: u32, name: &str) -> Result<(DirEntryMeta, Vec<u8>)> {
        let dir_inode = self.read_inode(dir_inode_num)?;
        if !dir_inode.is_dir() {
            return Err(Error::InvalidOperation);
        }
        let data = self.read_file_data(&dir_inode)?;
        for entry in self.parse_dir_entries(&data) {
            if entry.inode != 0 && entry.name == name {
                return Ok((entry, data));
            }
        }
        Err(Error::NotFound)
    }

    fn parse_dir_entries(&self, data: &[u8]) -> Vec<DirEntryMeta> {
        let mut entries = Vec::new();
        let mut off = 0usize;
        while off + 8 <= data.len() {
            let inode =
                u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
            let rec_len = u16::from_le_bytes([data[off + 4], data[off + 5]]) as usize;
            if rec_len == 0 || off + rec_len > data.len() {
                break;
            }
            let name_len = data[off + 6] as usize;
            if 8 + name_len <= rec_len {
                let name_bytes = &data[off + 8..off + 8 + name_len];
                let name = core::str::from_utf8(name_bytes)
                    .map(String::from)
                    .unwrap_or_else(|_| String::new());
                entries.push(DirEntryMeta {
                    offset: off,
                    inode,
                    rec_len,
                    name_len,
                    name,
                });
            }
            off += rec_len;
        }
        entries
    }

    fn add_dir_entry(
        &self,
        dir_inode_num: u32,
        name: &str,
        inode_num: u32,
        file_type: u8,
    ) -> Result<()> {
        if name.is_empty() || name.len() > 255 {
            return Err(Error::InvalidArgument);
        }
        let needed = dir_entry_len(name.len());
        let dir_inode = self.read_inode(dir_inode_num)?;
        let mut data = self.read_file_data(&dir_inode)?;
        let entries = self.parse_dir_entries(&data);

        for entry in &entries {
            if entry.inode == 0 && entry.rec_len >= needed {
                self.write_dir_record(
                    &mut data,
                    entry.offset,
                    inode_num,
                    entry.rec_len as u16,
                    name,
                    file_type,
                )?;
                self.write_file_by_num(dir_inode_num, 0, &data)?;
                return Ok(());
            }
        }

        for entry in &entries {
            if entry.inode == 0 {
                continue;
            }
            let used = dir_entry_len(entry.name_len);
            if entry.rec_len >= used + needed {
                data[entry.offset + 4..entry.offset + 6]
                    .copy_from_slice(&(used as u16).to_le_bytes());
                let new_off = entry.offset + used;
                let new_len = entry.rec_len - used;
                self.write_dir_record(
                    &mut data,
                    new_off,
                    inode_num,
                    new_len as u16,
                    name,
                    file_type,
                )?;
                self.write_file_by_num(dir_inode_num, 0, &data)?;
                return Ok(());
            }
        }

        let mut new_block = alloc::vec![0u8; self.block_size];
        self.write_dir_record(
            &mut new_block,
            0,
            inode_num,
            self.block_size as u16,
            name,
            file_type,
        )?;
        let old_size = data.len();
        self.write_file_by_num(dir_inode_num, old_size, &new_block)?;
        Ok(())
    }

    fn clear_dir_entry(&self, data: &mut [u8], offset: usize) {
        data[offset..offset + 4].copy_from_slice(&0u32.to_le_bytes());
    }

    fn can_rename_in_place(&self, data: &[u8], offset: usize, name: &str) -> bool {
        if offset + 8 > data.len() {
            return false;
        }
        let rec_len = u16::from_le_bytes([data[offset + 4], data[offset + 5]]) as usize;
        let need = dir_entry_len(name.len());
        rec_len >= need
    }

    fn update_dir_entry_name(&self, data: &mut [u8], offset: usize, name: &str) -> Result<()> {
        if !self.can_rename_in_place(data, offset, name) {
            return Err(Error::BufferTooSmall);
        }
        let name_len = name.len();
        data[offset + 6] = name_len as u8;
        let dst = &mut data[offset + 8..offset + 8 + name_len];
        dst.copy_from_slice(name.as_bytes());
        Ok(())
    }

    fn write_dir_record(
        &self,
        dst: &mut [u8],
        offset: usize,
        inode_num: u32,
        rec_len: u16,
        name: &str,
        file_type: u8,
    ) -> Result<()> {
        let name_len = name.len();
        if name_len > 255
            || offset + rec_len as usize > dst.len()
            || (rec_len as usize) < 8 + name_len
        {
            return Err(Error::InvalidArgument);
        }
        dst[offset..offset + 4].copy_from_slice(&inode_num.to_le_bytes());
        dst[offset + 4..offset + 6].copy_from_slice(&rec_len.to_le_bytes());
        dst[offset + 6] = name_len as u8;
        dst[offset + 7] = file_type;
        dst[offset + 8..offset + 8 + name_len].copy_from_slice(name.as_bytes());
        if offset + rec_len as usize > offset + 8 + name_len {
            dst[offset + 8 + name_len..offset + rec_len as usize].fill(0);
        }
        Ok(())
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

        // Fast path: cache hit — read the 4-byte pointer out of the cached copy.
        {
            let cache = self.indirect_cache.lock();
            if cache.block_num == block_num && !cache.contents.is_empty() {
                let off = index * 4;
                if off + 4 <= cache.contents.len() {
                    let bytes: [u8; 4] = cache.contents[off..off + 4].try_into().unwrap();
                    return Ok(u32::from_le_bytes(bytes));
                }
            }
        }

        // Miss: load the entire indirect block once and serve from cache.
        let block_offset = (block_num as usize) * self.block_size;
        let mut full_block = alloc::vec![0u8; self.block_size];
        self.block.read_bytes(block_offset as u64, &mut full_block)?;

        let off = index * 4;
        if off + 4 > full_block.len() {
            return Err(Error::InvalidArgument);
        }
        let bytes: [u8; 4] = full_block[off..off + 4].try_into().unwrap();
        let result = u32::from_le_bytes(bytes);

        // Replace cache slot. Fine to do this after the read returned;
        // no other thread mutates `indirect_cache` outside the mutex.
        let mut cache = self.indirect_cache.lock();
        cache.block_num = block_num;
        cache.contents = full_block;

        Ok(result)
    }

    fn write_indirect_block(&self, block_num: u32, index: usize, value: u32) -> Result<()> {
        if block_num == 0 {
            return Err(Error::InvalidArgument);
        }
        let offset = (block_num as usize) * self.block_size + index * 4;
        self.block
            .write_bytes(offset as u64, &value.to_le_bytes())?;
        // The on-disk indirect block just changed; if our cached copy was for
        // this same block, drop it so the next read re-fetches.
        self.invalidate_indirect_cache();
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

    fn allocate_inode(&self) -> Result<u32> {
        let groups = self.sb.inodes_count.div_ceil(self.sb.inodes_per_group);
        for group in 0..groups {
            let mut desc = self.read_group_desc(group)?;
            let free_inodes = u16::from_le_bytes([desc[14], desc[15]]);
            if free_inodes == 0 {
                continue;
            }
            let bitmap_block = u32::from_le_bytes([desc[4], desc[5], desc[6], desc[7]]);
            let bitmap_offset = (bitmap_block as usize) * self.block_size;
            let mut bitmap = alloc::vec![0u8; self.block_size];
            self.block.read_bytes(bitmap_offset as u64, &mut bitmap)?;

            for bit_idx in 0..self.sb.inodes_per_group as usize {
                let inode_num = group * self.sb.inodes_per_group + bit_idx as u32 + 1;
                if inode_num < self.sb.first_ino && inode_num != 2 {
                    continue;
                }
                if inode_num > self.sb.inodes_count {
                    break;
                }
                let byte = bit_idx / 8;
                let mask = 1u8 << (bit_idx % 8);
                if byte >= bitmap.len() {
                    break;
                }
                if bitmap[byte] & mask != 0 {
                    continue;
                }
                bitmap[byte] |= mask;
                self.block.write_bytes(bitmap_offset as u64, &bitmap)?;
                let new_group_free = free_inodes.saturating_sub(1);
                desc[14..16].copy_from_slice(&new_group_free.to_le_bytes());
                self.write_group_desc(group, &desc)?;
                self.dec_superblock_free_inodes()?;
                return Ok(inode_num);
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

    fn dec_superblock_free_inodes(&self) -> Result<()> {
        let mut sb_buf = [0u8; 1024];
        self.block.read_bytes(1024, &mut sb_buf)?;
        let cur = u32::from_le_bytes([sb_buf[16], sb_buf[17], sb_buf[18], sb_buf[19]]);
        let next = cur.saturating_sub(1);
        sb_buf[16..20].copy_from_slice(&next.to_le_bytes());
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
                name: e.name,
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

#[derive(Clone)]
struct DirEntryMeta {
    offset: usize,
    inode: u32,
    rec_len: usize,
    name_len: usize,
    name: String,
}

fn dir_entry_len(name_len: usize) -> usize {
    (8 + name_len + 3) & !3
}
