//! Filesystem abstractions and protocol helpers.
//!
//! This module provides:
//! - Plugin traits for block devices and filesystems (SOLID principles)
//! - VFS IPC protocol definitions
//! - Client helpers for VFS access

pub mod client;
pub mod protocol;
pub mod traits;

pub use client::{VfsClient, VfsDirEntry, VfsFile, VfsGrant, VfsReadRing, VfsReadRingChunk};
pub use protocol::{
    VfsOp, VFS_CLOSE, VFS_MAP_ELF, VFS_MKDIR, VFS_OPEN, VFS_READDIR, VFS_READ_GRANT, VFS_READ_RING,
    VFS_RENAME, VFS_RING_SETUP, VFS_RMDIR, VFS_UNLINK,
};
pub use traits::{BlockDevice, DirEntry, FileStat, Filesystem};
