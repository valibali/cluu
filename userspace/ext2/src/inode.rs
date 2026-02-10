//! Ext2 inode structures.

/// File type constants from i_mode.
pub const S_IFMT: u16 = 0xF000;
pub const S_IFREG: u16 = 0x8000; // Regular file
pub const S_IFDIR: u16 = 0x4000; // Directory
pub const S_IFLNK: u16 = 0xA000; // Symbolic link

/// Parsed ext2 inode.
#[derive(Debug, Clone)]
pub struct Inode {
    pub mode: u16,
    pub uid: u16,
    pub size_lo: u32,
    pub atime: u32,
    pub ctime: u32,
    pub mtime: u32,
    pub dtime: u32,
    pub gid: u16,
    pub links_count: u16,
    pub blocks: u32,
    pub flags: u32,
    pub direct_blocks: [u32; 12],
    pub indirect_block: u32,
    pub double_indirect: u32,
    pub triple_indirect: u32,
    pub size_hi: u32, // Only for regular files in rev >= 1
}

impl Inode {
    /// Parse inode from raw bytes.
    pub fn parse(data: &[u8]) -> Self {
        Self {
            mode: read_u16(data, 0),
            uid: read_u16(data, 2),
            size_lo: read_u32(data, 4),
            atime: read_u32(data, 8),
            ctime: read_u32(data, 12),
            mtime: read_u32(data, 16),
            dtime: read_u32(data, 20),
            gid: read_u16(data, 24),
            links_count: read_u16(data, 26),
            blocks: read_u32(data, 28),
            flags: read_u32(data, 32),
            direct_blocks: [
                read_u32(data, 40),
                read_u32(data, 44),
                read_u32(data, 48),
                read_u32(data, 52),
                read_u32(data, 56),
                read_u32(data, 60),
                read_u32(data, 64),
                read_u32(data, 68),
                read_u32(data, 72),
                read_u32(data, 76),
                read_u32(data, 80),
                read_u32(data, 84),
            ],
            indirect_block: read_u32(data, 88),
            double_indirect: read_u32(data, 92),
            triple_indirect: read_u32(data, 96),
            size_hi: if data.len() > 108 {
                read_u32(data, 108)
            } else {
                0
            },
        }
    }

    /// Check if this is a directory.
    pub fn is_dir(&self) -> bool {
        (self.mode & S_IFMT) == S_IFDIR
    }

    /// Check if this is a regular file.
    pub fn is_file(&self) -> bool {
        (self.mode & S_IFMT) == S_IFREG
    }

    /// Check if this is a symbolic link.
    pub fn is_symlink(&self) -> bool {
        (self.mode & S_IFMT) == S_IFLNK
    }

    /// Get the file size (handles 64-bit sizes for large files).
    pub fn size(&self) -> u64 {
        if self.is_file() {
            ((self.size_hi as u64) << 32) | (self.size_lo as u64)
        } else {
            self.size_lo as u64
        }
    }

    /// Update inode size fields.
    pub fn set_size(&mut self, size: u64) {
        self.size_lo = size as u32;
        self.size_hi = (size >> 32) as u32;
    }

    /// Serialize inode fields back to raw bytes.
    pub fn write_to(&self, data: &mut [u8]) {
        write_u16(data, 0, self.mode);
        write_u16(data, 2, self.uid);
        write_u32(data, 4, self.size_lo);
        write_u32(data, 8, self.atime);
        write_u32(data, 12, self.ctime);
        write_u32(data, 16, self.mtime);
        write_u32(data, 20, self.dtime);
        write_u16(data, 24, self.gid);
        write_u16(data, 26, self.links_count);
        write_u32(data, 28, self.blocks);
        write_u32(data, 32, self.flags);
        write_u32(data, 40, self.direct_blocks[0]);
        write_u32(data, 44, self.direct_blocks[1]);
        write_u32(data, 48, self.direct_blocks[2]);
        write_u32(data, 52, self.direct_blocks[3]);
        write_u32(data, 56, self.direct_blocks[4]);
        write_u32(data, 60, self.direct_blocks[5]);
        write_u32(data, 64, self.direct_blocks[6]);
        write_u32(data, 68, self.direct_blocks[7]);
        write_u32(data, 72, self.direct_blocks[8]);
        write_u32(data, 76, self.direct_blocks[9]);
        write_u32(data, 80, self.direct_blocks[10]);
        write_u32(data, 84, self.direct_blocks[11]);
        write_u32(data, 88, self.indirect_block);
        write_u32(data, 92, self.double_indirect);
        write_u32(data, 96, self.triple_indirect);
        if data.len() > 108 {
            write_u32(data, 108, self.size_hi);
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

fn write_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
