//! Filesystem abstractions and protocol helpers.
//!
//! This module provides:
//! - Plugin traits for block devices and filesystems (SOLID principles)
//! - VFS IPC protocol definitions
//! - Client helpers for VFS access

pub mod client;
pub mod protocol;
pub mod traits;

pub use client::{VfsClient, VfsDirEntry, VfsFile, VfsGrant};
pub use protocol::{VfsOp, VFS_CLOSE, VFS_MAP_ELF, VFS_OPEN, VFS_READ_GRANT, VFS_READDIR};
pub use traits::{BlockDevice, DirEntry, FileStat, Filesystem};
