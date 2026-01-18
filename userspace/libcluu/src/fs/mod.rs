//! Filesystem protocol helpers for userspace clients.
//!
//! This module exposes the VFS IPC protocol and a thin client wrapper
//! used by higher-level services and shell commands.

pub mod client;
pub mod protocol;

pub use client::{VfsClient, VfsFile, VfsGrant};
pub use protocol::{VfsOp, VFS_CLOSE, VFS_OPEN, VFS_READ_GRANT};
