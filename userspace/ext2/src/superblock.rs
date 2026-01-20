//! Ext2 superblock parsing.

use libcluu::{Error, Result};

/// Ext2 superblock magic number.
pub const EXT2_MAGIC: u16 = 0xEF53;

/// Parsed ext2 superblock.
#[derive(Debug, Clone)]
pub struct Superblock {
    pub inodes_count: u32,
    pub blocks_count: u32,
    pub free_blocks_count: u32,
    pub free_inodes_count: u32,
    pub first_data_block: u32,
    pub log_block_size: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub magic: u16,
    pub state: u16,
    pub rev_level: u32,
    pub first_ino: u32,
    pub inode_size: u16,
}

impl Superblock {
    /// Parse superblock from raw bytes (1024 bytes starting at offset 1024 on disk).
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 1024 {
            return Err(Error::InvalidArgument);
        }

        let magic = u16::from_le_bytes([data[56], data[57]]);
        if magic != EXT2_MAGIC {
            return Err(Error::InvalidArgument);
        }

        Ok(Self {
            inodes_count: read_u32(data, 0),
            blocks_count: read_u32(data, 4),
            free_blocks_count: read_u32(data, 12),
            free_inodes_count: read_u32(data, 16),
            first_data_block: read_u32(data, 20),
            log_block_size: read_u32(data, 24),
            blocks_per_group: read_u32(data, 32),
            inodes_per_group: read_u32(data, 40),
            magic,
            state: u16::from_le_bytes([data[58], data[59]]),
            rev_level: read_u32(data, 76),
            first_ino: read_u32(data, 84),
            inode_size: u16::from_le_bytes([data[88], data[89]]),
        })
    }
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}
