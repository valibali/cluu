//! Per-client file descriptor table for VFS.
//!
//! Each client is identified by a caller-provided ID (typically its registry
//! control endpoint token). This keeps FD namespaces isolated across processes.

use alloc::collections::BTreeMap;

#[derive(Clone, Copy)]
pub struct FileEntry {
    pub base: usize,
    pub offset: usize,
    pub size: usize,
}

struct ClientTable {
    next_fd: usize,
    entries: BTreeMap<usize, FileEntry>,
}

impl ClientTable {
    fn new() -> Self {
        Self {
            next_fd: 4,
            entries: BTreeMap::new(),
        }
    }
}

pub struct FdTable {
    clients: BTreeMap<usize, ClientTable>,
}

impl FdTable {
    pub fn new() -> Self {
        Self {
            clients: BTreeMap::new(),
        }
    }

    pub fn open(&mut self, client_id: usize, entry: FileEntry) -> usize {
        let table = self.clients.entry(client_id).or_insert_with(ClientTable::new);
        let fd = table.next_fd;
        table.next_fd += 1;
        table.entries.insert(fd, entry);
        fd
    }

    pub fn get(&self, client_id: usize, fd: usize) -> Option<FileEntry> {
        self.clients.get(&client_id)?.entries.get(&fd).copied()
    }

    pub fn close(&mut self, client_id: usize, fd: usize) {
        if let Some(table) = self.clients.get_mut(&client_id) {
            table.entries.remove(&fd);
        }
    }
}
