//! Mount table and path resolution for the VFS service.
//!
//! The table is intentionally minimal but structured for extension. Each
//! mountpoint resolves a path prefix to a backend. We start with the initrd
//! image mounted at `/dev/initrd` and keep the registration API for future
//! devices (e.g., block, procfs, tmpfs).

use alloc::vec::Vec;
use alloc::string::String;
use crate::fd_table::FileEntry;
use libcluu::tar::find_member;
use libcluu::{Error, Result};

pub const INITRD_MOUNT: &str = "/dev/initrd";

enum MountBackend<'a> {
    Initrd(&'a [u8]),
}

struct Mount<'a> {
    prefix: &'static str,
    backend: MountBackend<'a>,
}

pub struct MountTable<'a> {
    mounts: Vec<Mount<'a>>,
}

impl<'a> MountTable<'a> {
    pub fn new(initrd: &'a [u8]) -> Self {
        let mut table = Self { mounts: Vec::new() };
        table.add_initrd(INITRD_MOUNT, initrd);
        table
    }

    pub fn add_initrd(&mut self, prefix: &'static str, initrd: &'a [u8]) {
        self.mounts.push(Mount {
            prefix,
            backend: MountBackend::Initrd(initrd),
        });
    }

    pub fn open(&self, path: &str) -> Result<FileEntry> {
        let mount = resolve_mount(&self.mounts, path)?;
        match mount.backend {
            MountBackend::Initrd(initrd) => open_from_initrd(initrd, mount.prefix, path),
        }
    }
}

fn resolve_mount<'a>(mounts: &'a [Mount<'a>], path: &str) -> Result<&'a Mount<'a>> {
    let mut best: Option<&Mount<'a>> = None;
    for mount in mounts {
        if (path == mount.prefix || path.starts_with(mount.prefix) && path.as_bytes().get(mount.prefix.len()) == Some(&b'/'))
            && best.is_none_or(|current| mount.prefix.len() > current.prefix.len()) {
                best = Some(mount);
            }
    }
    best.ok_or(Error::NotFound)
}

fn open_from_initrd(initrd: &[u8], prefix: &str, path: &str) -> Result<FileEntry> {
    let rel_path = strip_prefix(path, prefix)?;
    let slice = find_member(initrd, rel_path)
        .or_else(|| find_member(initrd, &dot_prefixed(rel_path)))
        .ok_or(Error::NotFound)?;
    let base = initrd.as_ptr() as usize;
    let offset = slice.as_ptr() as usize - base;
    Ok(FileEntry {
        base,
        offset,
        size: slice.len(),
    })
}

fn strip_prefix<'a>(path: &'a str, prefix: &str) -> Result<&'a str> {
    let Some(rest) = path.strip_prefix(prefix) else {
        return Err(Error::NotFound);
    };
    let rel = rest.strip_prefix('/').unwrap_or("");
    if rel.is_empty() {
        return Err(Error::InvalidArgument);
    }
    Ok(rel)
}

fn dot_prefixed(path: &str) -> String {
    let mut prefixed = String::with_capacity(path.len() + 2);
    prefixed.push_str("./");
    prefixed.push_str(path);
    prefixed
}
