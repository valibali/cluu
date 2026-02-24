//! Inode-based in-memory filesystem for per-container ephemeral storage.
//!
//! Pure data structure — no IPC, no VFS protocol, no view logic.
//! Quota-enforced: tracks total used bytes and rejects writes/creates that
//! would exceed the limit.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::Error;

type Result<T> = core::result::Result<T, Error>;

/// Content stored in an inode.
#[derive(Clone)]
pub enum InodeContent {
    File { data: Vec<u8> },
    Dir { children: BTreeMap<String, usize> },
}

/// A single inode in the filesystem.
pub struct Inode {
    pub content: InodeContent,
    /// Bytes charged against the quota for this inode.
    pub charged_bytes: usize,
}

/// In-memory filesystem with inode allocation, directory tree, and quota.
pub struct MemFs {
    inodes: Vec<Option<Inode>>,
    free_list: Vec<usize>,
    quota_bytes: usize,
    used_bytes: usize,
}

impl MemFs {
    /// Create a new empty filesystem with the given quota.
    /// Inode 0 = root directory.
    pub fn new(quota_bytes: usize) -> Self {
        let root = Inode {
            content: InodeContent::Dir {
                children: BTreeMap::new(),
            },
            charged_bytes: 0,
        };
        Self {
            inodes: alloc::vec![Some(root)],
            free_list: Vec::new(),
            quota_bytes,
            used_bytes: 0,
        }
    }

    /// Open a file by path. Returns (inode_id, size).
    pub fn open(&self, path: &str) -> Result<(usize, usize)> {
        let ino = self.resolve(path)?;
        let inode = self.get_inode(ino)?;
        match &inode.content {
            InodeContent::File { data } => Ok((ino, data.len())),
            InodeContent::Dir { .. } => Err(Error::InvalidOperation),
        }
    }

    /// Read `len` bytes starting at `offset` from an inode.
    pub fn read(&self, inode_id: usize, offset: usize, len: usize) -> Result<Vec<u8>> {
        let inode = self.get_inode(inode_id)?;
        match &inode.content {
            InodeContent::File { data } => {
                let available = data.len().saturating_sub(offset);
                let n = len.min(available);
                if n == 0 {
                    return Ok(Vec::new());
                }
                Ok(data[offset..offset + n].to_vec())
            }
            InodeContent::Dir { .. } => Err(Error::InvalidOperation),
        }
    }

    /// Write `data` at `offset` in the given inode. Returns bytes written.
    pub fn write(&mut self, inode_id: usize, offset: usize, data: &[u8]) -> Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset.checked_add(data.len()).ok_or(Error::Overflow)?;
        let inode = self.inodes.get_mut(inode_id)
            .and_then(|s| s.as_mut())
            .ok_or(Error::NotFound)?;
        if let InodeContent::File { data: ref mut d } = inode.content {
            if end > d.len() {
                let delta = end - d.len();
                if self.used_bytes + delta > self.quota_bytes {
                    return Err(Error::Overflow);
                }
                d.resize(end, 0);
                inode.charged_bytes += delta;
                self.used_bytes += delta;
            }
            d[offset..end].copy_from_slice(data);
            Ok(data.len())
        } else {
            Err(Error::InvalidOperation)
        }
    }

    /// Create a new empty file at `path`. Returns the inode id.
    pub fn create(&mut self, path: &str) -> Result<usize> {
        let (parent_ino, name) = self.resolve_parent(path)?;
        // Check name doesn't already exist.
        if let InodeContent::Dir { ref children } = self.get_inode(parent_ino)?.content {
            if children.contains_key(name) {
                return Err(Error::AlreadyExists);
            }
        } else {
            return Err(Error::InvalidOperation);
        }
        let ino = self.alloc_inode(Inode {
            content: InodeContent::File { data: Vec::new() },
            charged_bytes: 0,
        });
        if let InodeContent::Dir { ref mut children } = self.get_inode_mut(parent_ino)?.content {
            children.insert(String::from(name), ino);
        }
        Ok(ino)
    }

    /// Create a directory at `path`.
    pub fn mkdir(&mut self, path: &str) -> Result<()> {
        let (parent_ino, name) = self.resolve_parent(path)?;
        if let InodeContent::Dir { ref children } = self.get_inode(parent_ino)?.content {
            if children.contains_key(name) {
                return Err(Error::AlreadyExists);
            }
        } else {
            return Err(Error::InvalidOperation);
        }
        let ino = self.alloc_inode(Inode {
            content: InodeContent::Dir {
                children: BTreeMap::new(),
            },
            charged_bytes: 0,
        });
        if let InodeContent::Dir { ref mut children } = self.get_inode_mut(parent_ino)?.content {
            children.insert(String::from(name), ino);
        }
        Ok(())
    }

    /// Remove a file at `path`.
    pub fn unlink(&mut self, path: &str) -> Result<()> {
        let (parent_ino, name) = self.resolve_parent(path)?;
        let child_ino = self.lookup_child(parent_ino, name)?;
        // Must be a file.
        let charged = {
            let inode = self.get_inode(child_ino)?;
            match &inode.content {
                InodeContent::File { .. } => inode.charged_bytes,
                InodeContent::Dir { .. } => return Err(Error::InvalidOperation),
            }
        };
        self.used_bytes = self.used_bytes.saturating_sub(charged);
        self.free_inode(child_ino);
        if let InodeContent::Dir { ref mut children } = self.get_inode_mut(parent_ino)?.content {
            children.remove(name);
        }
        Ok(())
    }

    /// Remove an empty directory at `path`.
    pub fn rmdir(&mut self, path: &str) -> Result<()> {
        let (parent_ino, name) = self.resolve_parent(path)?;
        let child_ino = self.lookup_child(parent_ino, name)?;
        // Must be an empty dir.
        {
            let inode = self.get_inode(child_ino)?;
            match &inode.content {
                InodeContent::Dir { children } => {
                    if !children.is_empty() {
                        return Err(Error::InvalidOperation);
                    }
                }
                InodeContent::File { .. } => return Err(Error::InvalidOperation),
            }
        }
        self.free_inode(child_ino);
        if let InodeContent::Dir { ref mut children } = self.get_inode_mut(parent_ino)?.content {
            children.remove(name);
        }
        Ok(())
    }

    /// List directory entries. Returns (name, is_dir) pairs.
    pub fn readdir(&self, path: &str) -> Result<Vec<(String, bool)>> {
        let ino = self.resolve(path)?;
        let inode = self.get_inode(ino)?;
        match &inode.content {
            InodeContent::Dir { children } => {
                let mut entries = Vec::new();
                for (name, &child_ino) in children {
                    let is_dir = matches!(
                        self.get_inode(child_ino).map(|i| &i.content),
                        Ok(InodeContent::Dir { .. })
                    );
                    entries.push((name.clone(), is_dir));
                }
                Ok(entries)
            }
            InodeContent::File { .. } => Err(Error::InvalidOperation),
        }
    }

    /// Rename a file or directory.
    pub fn rename(&mut self, old_path: &str, new_path: &str) -> Result<()> {
        let (old_parent, old_name) = self.resolve_parent(old_path)?;
        let child_ino = self.lookup_child(old_parent, old_name)?;
        let old_name_owned = String::from(old_name);

        let (new_parent, new_name) = self.resolve_parent(new_path)?;
        let new_name_owned = String::from(new_name);

        // Remove from old parent.
        if let InodeContent::Dir { ref mut children } = self.get_inode_mut(old_parent)?.content {
            children.remove(&old_name_owned);
        }
        // If destination exists, remove it first (overwrite semantics).
        if let Ok(existing) = self.lookup_child(new_parent, &new_name_owned) {
            let charged = self.get_inode(existing).map(|i| i.charged_bytes).unwrap_or(0);
            self.used_bytes = self.used_bytes.saturating_sub(charged);
            self.free_inode(existing);
        }
        // Insert into new parent.
        if let InodeContent::Dir { ref mut children } = self.get_inode_mut(new_parent)?.content {
            children.insert(new_name_owned, child_ino);
        }
        Ok(())
    }

    /// Hard links are not supported.
    pub fn link(&self, _old_path: &str, _new_path: &str) -> Result<()> {
        Err(Error::NotImplemented)
    }

    /// Truncate a file to the given size.
    pub fn truncate(&mut self, inode_id: usize, size: usize) -> Result<()> {
        let inode = self.inodes.get_mut(inode_id)
            .and_then(|s| s.as_mut())
            .ok_or(Error::NotFound)?;
        if let InodeContent::File { ref mut data } = inode.content {
            let old_size = data.len();
            if size < old_size {
                let freed = old_size - size;
                data.truncate(size);
                inode.charged_bytes = inode.charged_bytes.saturating_sub(freed);
                self.used_bytes = self.used_bytes.saturating_sub(freed);
            } else if size > old_size {
                let delta = size - old_size;
                if self.used_bytes + delta > self.quota_bytes {
                    return Err(Error::Overflow);
                }
                data.resize(size, 0);
                inode.charged_bytes += delta;
                self.used_bytes += delta;
            }
            Ok(())
        } else {
            Err(Error::InvalidOperation)
        }
    }

    /// Return the size of a file inode.
    pub fn file_size(&self, inode_id: usize) -> usize {
        self.get_inode(inode_id)
            .map(|i| match &i.content {
                InodeContent::File { data } => data.len(),
                InodeContent::Dir { .. } => 0,
            })
            .unwrap_or(0)
    }

    /// Total bytes used across all files.
    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Drop all inodes and reclaim all memory.
    pub fn drop_all(&mut self) {
        self.inodes.clear();
        self.free_list.clear();
        self.used_bytes = 0;
        // Re-create root dir.
        self.inodes.push(Some(Inode {
            content: InodeContent::Dir {
                children: BTreeMap::new(),
            },
            charged_bytes: 0,
        }));
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Resolve a path to an inode id, starting from root (inode 0).
    fn resolve(&self, path: &str) -> Result<usize> {
        let mut current = 0usize; // root
        for component in path.split('/').filter(|s| !s.is_empty()) {
            current = self.lookup_child(current, component)?;
        }
        Ok(current)
    }

    /// Resolve the parent directory and return (parent_inode, basename).
    fn resolve_parent<'a>(&self, path: &'a str) -> Result<(usize, &'a str)> {
        let path = path.trim_start_matches('/');
        let (parent_path, name) = match path.rfind('/') {
            Some(pos) => (&path[..pos], &path[pos + 1..]),
            None => ("", path),
        };
        if name.is_empty() {
            return Err(Error::InvalidArgument);
        }
        let parent_ino = self.resolve(parent_path)?;
        Ok((parent_ino, name))
    }

    fn lookup_child(&self, parent_ino: usize, name: &str) -> Result<usize> {
        let inode = self.get_inode(parent_ino)?;
        match &inode.content {
            InodeContent::Dir { children } => {
                children.get(name).copied().ok_or(Error::NotFound)
            }
            InodeContent::File { .. } => Err(Error::InvalidOperation),
        }
    }

    fn get_inode(&self, id: usize) -> Result<&Inode> {
        self.inodes
            .get(id)
            .and_then(|slot| slot.as_ref())
            .ok_or(Error::NotFound)
    }

    fn get_inode_mut(&mut self, id: usize) -> Result<&mut Inode> {
        self.inodes
            .get_mut(id)
            .and_then(|slot| slot.as_mut())
            .ok_or(Error::NotFound)
    }

    fn alloc_inode(&mut self, inode: Inode) -> usize {
        if let Some(id) = self.free_list.pop() {
            self.inodes[id] = Some(inode);
            id
        } else {
            let id = self.inodes.len();
            self.inodes.push(Some(inode));
            id
        }
    }

    fn free_inode(&mut self, id: usize) {
        if id < self.inodes.len() {
            self.inodes[id] = None;
            self.free_list.push(id);
        }
    }
}
