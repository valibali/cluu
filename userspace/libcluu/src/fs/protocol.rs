//! VFS IPC protocol definitions.
//!
//! The protocol is intentionally minimal. Messages use the shared `Message`
//! struct and encode arguments into the word slots. Any additional payload
//! (such as path strings) is appended after the message header.

/// Open a path and return a file descriptor + size.
pub const VFS_OPEN: u32 = 0x200;
/// Close a file descriptor.
pub const VFS_CLOSE: u32 = 0x201;
/// Read using zero-copy grant into the caller address space.
pub const VFS_READ_GRANT: u32 = 0x202;

/// Structured enum for protocol routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsOp {
    Open,
    Close,
    ReadGrant,
}

impl VfsOp {
    pub fn from_label(label: u32) -> Option<Self> {
        match label {
            VFS_OPEN => Some(Self::Open),
            VFS_CLOSE => Some(Self::Close),
            VFS_READ_GRANT => Some(Self::ReadGrant),
            _ => None,
        }
    }
}
