#![allow(unused)]

// Ext2 directory entry parsing.
use alloc::string::String;

/// Directory entry file types.
pub const FT_UNKNOWN: u8 = 0;
pub const FT_REG_FILE: u8 = 1;
pub const FT_DIR: u8 = 2;
pub const FT_CHRDEV: u8 = 3;
pub const FT_BLKDEV: u8 = 4;
pub const FT_FIFO: u8 = 5;
pub const FT_SOCK: u8 = 6;
pub const FT_SYMLINK: u8 = 7;

/// A raw parsed directory entry (internal to ext2).
#[derive(Debug, Clone)]
pub struct RawDirEntry {
    pub inode: u32,
    pub file_type: u8,
    pub name: String,
}

impl RawDirEntry {
    pub fn is_dir(&self) -> bool {
        self.file_type == FT_DIR
    }

    pub fn is_file(&self) -> bool {
        self.file_type == FT_REG_FILE
    }
}

/// Iterator over directory entries in raw directory data.
pub struct DirIter<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> DirIter<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
}

impl<'a> Iterator for DirIter<'a> {
    type Item = RawDirEntry;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.offset + 8 > self.data.len() {
                return None;
            }

            let inode = read_u32(self.data, self.offset);
            let rec_len = read_u16(self.data, self.offset + 4) as usize;
            let name_len = self.data[self.offset + 6] as usize;
            let file_type = self.data[self.offset + 7];

            if rec_len == 0 {
                return None;
            }

            let name_start = self.offset + 8;
            let name_end = name_start + name_len;

            self.offset += rec_len;

            // Skip deleted entries (inode == 0)
            if inode == 0 {
                continue;
            }

            if name_end > self.data.len() {
                return None;
            }

            let name_bytes = &self.data[name_start..name_end];
            let name = core::str::from_utf8(name_bytes)
                .map(String::from)
                .unwrap_or_else(|_| String::new());

            return Some(RawDirEntry {
                inode,
                file_type,
                name,
            });
        }
    }
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}
