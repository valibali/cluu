//! VFS IPC protocol definitions.
//!
//! The protocol is intentionally minimal. Messages use the shared `Message`
//! struct and encode arguments into the word slots. Any additional payload
//! (such as path strings or grant bases) is appended after the message header.
//!
//! Request layout (common):
//! - words[0] = payload length (bytes)
//! - words[1] = client_id (registry control endpoint token)
//!
//! OPEN:
//! - payload: path bytes
//! - reply: words[1] = fd, words[2] = size
//!
//! CLOSE:
//! - words[2] = fd
//!
//! READ_GRANT:
//! - words[2] = fd
//! - words[3] = offset
//! - words[4] = len
//! - words[5] = target_space_token
//! - payload: target_base (usize)
//! - reply: words[1] = len, words[2] = page_offset
//!
//! MAP_ELF:
//! - words[2] = fd
//! - words[3] = target_space_token
//! - reply: words[1] = entry_point, words[2] = file_size

/// Open a path and return a file descriptor + size.
pub const VFS_OPEN: u32 = 0x200;
/// Close a file descriptor.
pub const VFS_CLOSE: u32 = 0x201;
/// Read using zero-copy grant into the caller address space.
pub const VFS_READ_GRANT: u32 = 0x202;
/// Read directory entries.
pub const VFS_READDIR: u32 = 0x203;
/// Map ELF segments into a target address space.
pub const VFS_MAP_ELF: u32 = 0x204;
/// Write data to a file descriptor.
pub const VFS_WRITE: u32 = 0x205;
/// Stat a path.
pub const VFS_STAT: u32 = 0x206;
/// Stat an open file descriptor.
pub const VFS_FSTAT: u32 = 0x207;

/// Structured enum for protocol routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsOp {
    Open,
    Close,
    ReadGrant,
    Readdir,
    MapElf,
    Write,
    Stat,
    Fstat,
}

impl VfsOp {
    pub fn from_label(label: u32) -> Option<Self> {
        match label {
            VFS_OPEN => Some(Self::Open),
            VFS_CLOSE => Some(Self::Close),
            VFS_READ_GRANT => Some(Self::ReadGrant),
            VFS_READDIR => Some(Self::Readdir),
            VFS_MAP_ELF => Some(Self::MapElf),
            VFS_WRITE => Some(Self::Write),
            VFS_STAT => Some(Self::Stat),
            VFS_FSTAT => Some(Self::Fstat),
            _ => None,
        }
    }
}
