#![no_std]
#![no_main]

//! Virtual Filesystem Service for CLUU.
//!
//! Mount points are declared declaratively in `setup_mounts()`.
//! All path routing is handled by the unified MountTable.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::vec::Vec;
use core::mem::size_of;
use libcluu::elf::{ElfFile, LoadableSegment};
use libcluu::fs::protocol::{VfsOp, VFS_CLOSE, VFS_MAP_ELF, VFS_OPEN, VFS_READDIR, VFS_READ_GRANT};
use libcluu::ipc::{self, extract_reply_token, reply_with_payload};
use libcluu::types::Message;
use libcluu::*;

mod fd_table;
mod mount;
mod procfs;

use fd_table::{FdTable, OpenFile};
use mount::MountTable;

const SVC_TOKEN_LISTEN: usize = 7;
const IPC_MESSAGE_MAX: usize = 256;
/// Remote filesystem IPC label for zero-copy reads into the VFS grant buffer.
const FS_READ_GRANT: u32 = 0x306;
const USIZE_BYTES: usize = size_of::<usize>();
const TWO_USIZE_BYTES: usize = size_of::<usize>() * 2;

/// Buffer base for file data reads (shared grant window).
const READ_BUF_BASE: usize = 0x60000000;
/// Size of the shared grant window in the VFS address space.
/// 1MB for good throughput - reduces IPC round-trips for large files.
const GRANT_BUF_SIZE: usize = 1024 * 1024;
/// Buffer base for the VFS read cache region.
const CACHE_BUF_BASE: usize = 0x64000000;
/// Size of the VFS read cache region.
const CACHE_BUF_SIZE: usize = 32 * 1024 * 1024;
/// Cap for remote grant reads to avoid large transient allocations.
const REMOTE_READ_CAP: usize = GRANT_BUF_SIZE;
/// Maximum size of file to cache (8MB - covers most executables).
const FILE_CACHE_MAX_SIZE: usize = 8 * 1024 * 1024;
/// Maximum total cache size (8MB).
const FILE_CACHE_TOTAL_MAX: usize = 32 * 1024 * 1024;
const VFS_TRACE: bool = false;

macro_rules! vfs_trace {
    ($($arg:tt)*) => {
        if VFS_TRACE {
            let _ = debug_print(&format!($($arg)*));
        }
    };
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if let Err(err) = run_vfs() {
        let _ = debug_print(&format!("vfs: fatal error {:?}", err));
        return 1;
    }
    0
}

fn run_vfs() -> Result<()> {
    debug_print("vfs: starting...")?;

    let info = process_info();
    let endpoint = info.tokens[SVC_TOKEN_LISTEN];
    let space_token = info.tokens[TOKEN_SPACE];
    let initrd_size = info.params[PARAM_INITRD_SIZE] as usize;
    let initrd = map_initrd_slice(initrd_size);

    debug_print("vfs: registering...")?;
    registry::init("vfs")?;
    registry::register_default_outputs()?;
    registry::register_output("main", endpoint)?;

    debug_print("vfs: waiting for services...")?;

    // Wait for services to start
    for _ in 0..100 {
        yield_cpu()?;
    }

    // Setup all mount points declaratively
    let mounts = setup_mounts(initrd)?;

    let grant_buf_base = map_grant_buffer(space_token)?;
    let _ = debug_print(&format!(
        "vfs: grant buffer mapped base={:#x} size={}",
        grant_buf_base, GRANT_BUF_SIZE
    ));
    let cache_buf_base = map_cache_buffer(space_token)?;
    let _ = debug_print(&format!(
        "vfs: cache buffer mapped base={:#x} size={}",
        cache_buf_base, CACHE_BUF_SIZE
    ));
    let vfs_space_map_token =
        token_derive(space_token, Rights::SPACE_MAP.bits() as usize, u64::MAX)?;
    let mut server = VfsServer::new(
        endpoint,
        space_token,
        vfs_space_map_token,
        grant_buf_base,
        GRANT_BUF_SIZE,
        cache_buf_base,
        CACHE_BUF_SIZE,
        mounts,
    );
    let registry_endpoint = registry::control_endpoint();
    let mut buf = [0u8; IPC_MESSAGE_MAX];

    debug_print("vfs: ready")?;

    loop {
        let tokens = [endpoint, registry_endpoint];
        let (index, len) = libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX)?;
        if let Some((msg, payload)) = parse_message(&buf[..len]) {
            if index == 1 {
                let _ = registry::handle_incoming_message(&msg, payload);
                continue;
            }
            if let Err(err) = server.handle_message(&msg, payload) {
                vfs_trace!("vfs: handler error {:?}", err);
            }
        }
    }
}

/// Declarative mount point configuration.
///
/// All mount points are defined here in one place.
fn setup_mounts(initrd: &'static [u8]) -> Result<MountTable> {
    debug_print("vfs: setup_mounts start")?;
    let mut mounts = MountTable::new();

    // ═══════════════════════════════════════════════════════════════════════
    // Mount points configuration
    // ═══════════════════════════════════════════════════════════════════════

    // Initrd: system files from boot archive
    mounts.mount_initrd("/dev/initrd", initrd);
    debug_print("vfs: initrd mounted")?;

    // Ext2 filesystem: forwarded to virtio-blk service (mounted as root).
    let blkdev_endpoint = registry::subscribe_output("blkdev", "main")?;
    mounts.mount_remote("/", blkdev_endpoint, "blkdev");
    debug_print("vfs: mounted / (blkdev)")?;

    // Procfs: virtual filesystem with system information
    mounts.mount_virtual("/proc", "procfs", procfs::ENTRIES);
    debug_print("vfs: mounted /proc (procfs)")?;

    // ═══════════════════════════════════════════════════════════════════════
    // Future mount points can be added here:
    // - mounts.mount_virtual("/sys", "sysfs", sysfs::ENTRIES);
    // - mounts.mount_remote("/net", netfs_endpoint, "netfs");
    // - mounts.mount_initrd("/boot", boot_archive);
    // ═══════════════════════════════════════════════════════════════════════

    Ok(mounts)
}

fn map_initrd_slice(initrd_size: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, initrd_size) }
}

/// Simple LRU-ish file cache for ext2 files.
/// Caches entire file contents by (inode, size) to avoid repeated disk reads.
struct FileCache {
    /// Map from (inode, size) to cached entry.
    entries: BTreeMap<(u32, usize), CacheEntry>,
    /// Total bytes currently cached.
    total_size: usize,
    /// Access counter for LRU ordering.
    access_counter: usize,
    /// Dedicated cache region backing the entries.
    region: CacheRegion,
}

#[derive(Clone, Copy)]
struct CacheEntry {
    base: usize,
    len: usize,
    inode: u32,
    size: usize,
    last_access: usize,
}

struct CacheRegion {
    base: usize,
    size: usize,
    offset: usize,
    free: Vec<FreeBlock>,
}

struct FreeBlock {
    base: usize,
    size: usize,
}

impl FileCache {
    fn new(base: usize, size: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            total_size: 0,
            access_counter: 0,
            region: CacheRegion::new(base, size),
        }
    }

    fn get(&mut self, inode: u32, size: usize) -> Option<CacheEntry> {
        let key = (inode, size);
        if let Some(entry) = self.entries.get_mut(&key) {
            self.access_counter += 1;
            entry.last_access = self.access_counter;
            Some(*entry)
        } else {
            None
        }
    }

    fn reserve(&mut self, size: usize) -> Option<usize> {
        if size == 0 || size > FILE_CACHE_MAX_SIZE || size > FILE_CACHE_TOTAL_MAX {
            return None;
        }
        loop {
            if self.total_size + size > FILE_CACHE_TOTAL_MAX {
                if self.entries.is_empty() {
                    return None;
                }
                self.evict_lru();
                continue;
            }
            if let Some(base) = self.region.allocate(size) {
                return Some(base);
            }
            if self.entries.is_empty() {
                return None;
            }
            self.evict_lru();
        }
    }

    fn insert(&mut self, inode: u32, size: usize, base: usize, len: usize) {
        self.access_counter += 1;
        self.total_size += len;
        self.entries.insert(
            (inode, size),
            CacheEntry {
                base,
                len,
                inode,
                size,
                last_access: self.access_counter,
            },
        );
    }

    fn release_reserved(&mut self, base: usize, len: usize) {
        self.region.free(base, len);
    }

    fn remove(&mut self, inode: u32, size: usize) {
        let key = (inode, size);
        if let Some(entry) = self.entries.remove(&key) {
            self.total_size = self.total_size.saturating_sub(entry.len);
            self.region.free(entry.base, entry.len);
        }
    }

    fn evict_lru(&mut self) {
        // Find entry with lowest access counter
        let lru_path = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.last_access)
            .map(|(k, _)| *k);

        if let Some(path) = lru_path {
            if let Some(entry) = self.entries.remove(&path) {
                self.total_size = self.total_size.saturating_sub(entry.len);
                self.region.free(entry.base, entry.len);
            }
        }
    }
}

impl CacheRegion {
    fn new(base: usize, size: usize) -> Self {
        Self {
            base,
            size,
            offset: 0,
            free: Vec::new(),
        }
    }

    fn allocate(&mut self, size: usize) -> Option<usize> {
        let size = align_up(size, 16);
        if let Some(index) = self.free.iter().position(|b| b.size >= size) {
            let block = self.free.remove(index);
            let base = block.base;
            let remaining = block.size - size;
            if remaining > 0 {
                self.free.push(FreeBlock {
                    base: block.base + size,
                    size: remaining,
                });
            }
            return Some(base);
        }

        if self.offset + size > self.size {
            return None;
        }
        let base = self.base + self.offset;
        self.offset += size;
        Some(base)
    }

    fn free(&mut self, base: usize, size: usize) {
        let size = align_up(size, 16);
        self.free.push(FreeBlock { base, size });
        self.coalesce();
    }

    fn coalesce(&mut self) {
        if self.free.len() < 2 {
            return;
        }
        self.free.sort_by_key(|b| b.base);
        let mut merged: Vec<FreeBlock> = Vec::with_capacity(self.free.len());
        for block in self.free.drain(..) {
            if let Some(last) = merged.last_mut() {
                if last.base + last.size == block.base {
                    last.size += block.size;
                    continue;
                }
            }
            merged.push(block);
        }
        self.free = merged;
    }
}

fn align_up(value: usize, align: usize) -> usize {
    if align == 0 {
        return value;
    }
    (value + align - 1) & !(align - 1)
}

struct VfsServer {
    endpoint: usize,
    space_token: usize,
    vfs_space_map_token: usize,
    grant_buf_base: usize,
    grant_buf_size: usize,
    mounts: MountTable,
    files: FdTable,
    cache: FileCache,
}

impl VfsServer {
    fn new(
        endpoint: usize,
        space_token: usize,
        vfs_space_map_token: usize,
        grant_buf_base: usize,
        grant_buf_size: usize,
        cache_buf_base: usize,
        cache_buf_size: usize,
        mounts: MountTable,
    ) -> Self {
        Self {
            endpoint,
            space_token,
            vfs_space_map_token,
            grant_buf_base,
            grant_buf_size,
            mounts,
            files: FdTable::new(),
            cache: FileCache::new(cache_buf_base, cache_buf_size),
        }
    }

    fn handle_message(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        let Some(op) = VfsOp::from_label(msg.tag.label) else {
            vfs_trace!("vfs: unknown op");
            return Ok(());
        };
        let reply_token = extract_reply_token(msg).unwrap_or(self.endpoint);
        vfs_trace!("vfs: handling {:?} reply_token={}", op, reply_token);
        let result = match op {
            VfsOp::Open => self.handle_open(msg, payload, reply_token),
            VfsOp::Close => self.handle_close(msg, reply_token),
            VfsOp::ReadGrant => self.handle_read_grant(msg, payload, reply_token),
            VfsOp::Readdir => self.handle_readdir(payload, reply_token),
            VfsOp::MapElf => self.handle_map_elf(msg, reply_token),
        };
        vfs_trace!("vfs: handled {:?} result={:?}", op, result);
        result
    }

    fn handle_open(&mut self, msg: &Message, payload: &[u8], reply_token: usize) -> Result<()> {
        let client_id = msg.words[1];
        let mut reply_msg = Message::new(VFS_OPEN, [0; 6], 3);

        let path = match core::str::from_utf8(payload) {
            Ok(path) => path,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        // #region agent log
        let _ = debug_print(&format!("vfs: open '{}' client={}", path, client_id));
        // #endregion

        // Use unified mount table for all paths
        match self.mounts.open(path) {
            Ok(file) => {
                let size = file.size();
                let fd = self.files.open(client_id, file);
                reply_msg.words[0] = 0;
                reply_msg.words[1] = fd;
                reply_msg.words[2] = size;
                // #region agent log
                let _ = debug_print(&format!("vfs: open OK fd={} size={}", fd, size));
                // #endregion
            }
            Err(err) => {
                // #region agent log
                let _ = debug_print(&format!("vfs: open FAILED {:?}", err));
                // #endregion
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }

        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn handle_close(&mut self, msg: &Message, reply_token: usize) -> Result<()> {
        let client_id = msg.words[1];
        let fd = msg.words[2];
        self.files.close(client_id, fd);
        let mut reply_msg = Message::new(VFS_CLOSE, [0; 6], 1);
        reply_msg.words[0] = 0;
        if let Err(err) = ipc::reply(reply_token, &reply_msg, IpcFlags::empty()) {
            vfs_trace!("vfs: close reply failed {:?}", err);
        }
        Ok(())
    }

    fn handle_read_grant(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
    ) -> Result<()> {
        let client_id = msg.words[1];
        let fd = msg.words[2];
        let offset = msg.words[3];
        let requested = msg.words[4];
        let mut reply_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);
        vfs_trace!(
            "vfs: read_grant start client={} fd={} off={} req={}",
            client_id,
            fd,
            offset,
            requested
        );

        let Some((target_base, target_space)) = parse_usize_pair(payload) else {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };

        let Some(file) = self.files.get(client_id, fd).cloned() else {
            reply_msg.words[0] = Error::NotFound.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };

        if requested == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        if target_base & (PAGE_SIZE - 1) != 0 {
            reply_msg.words[0] = Error::InvalidArgument as isize as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        // #region agent log
        let _ = debug_print(&format!(
            "vfs: read_grant target_base={:#x} target_space={}",
            target_base, target_space
        ));
        // #endregion

        match file {
            OpenFile::Memory(entry) => {
                // #region agent log
                let _ = debug_print("vfs: read_grant from Memory");
                // #endregion
                self.read_grant_memory(
                    &entry,
                    offset,
                    requested,
                    target_base,
                    target_space,
                    &mut reply_msg,
                )?;
            }
            OpenFile::Ext2(entry) => {
                // #region agent log
                let _ = debug_print(&format!(
                    "vfs: read_grant from Ext2 inode={} size={}",
                    entry.inode, entry.size
                ));
                // #endregion
                if let Some(cache_entry) = self.cache.get(entry.inode, entry.size) {
                    // #region agent log
                    let _ = debug_print(&format!(
                        "vfs: cache hit inode={} size={}",
                        entry.inode, entry.size
                    ));
                    // #endregion
                    self.read_grant_cached_region(
                        cache_entry.base,
                        cache_entry.len,
                        offset,
                        requested,
                        target_base,
                        target_space,
                        &mut reply_msg,
                    )?;
                } else if offset == 0 && requested >= entry.size {
                    // #region agent log
                    let _ = debug_print(&format!(
                        "vfs: cache miss, filling inode={} size={}",
                        entry.inode, entry.size
                    ));
                    // #endregion
                    if let Some(cache_entry) = self.cache_ext2_file(&entry) {
                        // #region agent log
                        let _ = debug_print(&format!(
                            "vfs: cache fill ok inode={} size={}",
                            entry.inode, entry.size
                        ));
                        // #endregion
                        self.read_grant_cached_region(
                            cache_entry.base,
                            cache_entry.len,
                            offset,
                            requested,
                            target_base,
                            target_space,
                            &mut reply_msg,
                        )?;
                    } else {
                        // #region agent log
                        let _ = debug_print(&format!(
                            "vfs: cache fill skipped inode={} size={}",
                            entry.inode, entry.size
                        ));
                        // #endregion
                        self.read_grant_remote_chunked(
                            entry.endpoint,
                            entry.inode,
                            entry.size,
                            offset,
                            requested,
                            target_base,
                            target_space,
                            &mut reply_msg,
                        )?;
                    }
                } else {
                    // Direct chunked reads from disk when cache is unavailable.
                    self.read_grant_remote_chunked(
                        entry.endpoint,
                        entry.inode,
                        entry.size,
                        offset,
                        requested,
                        target_base,
                        target_space,
                        &mut reply_msg,
                    )?;
                }
            }
            OpenFile::Virtual(vfile) => {
                // #region agent log
                let _ = debug_print("vfs: read_grant from Virtual");
                // #endregion
                self.read_grant_virtual(
                    &vfile.data,
                    offset,
                    requested,
                    target_base,
                    target_space,
                    &mut reply_msg,
                )?;
            }
        }

        // #region agent log
        let _ = debug_print(&format!(
            "vfs: read_grant done status={} len={}",
            reply_msg.words[0], reply_msg.words[1]
        ));
        // #endregion

        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn read_grant_memory(
        &self,
        entry: &fd_table::FileEntry,
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = entry.size.saturating_sub(offset);
        let len = requested.min(available).min(REMOTE_READ_CAP);
        vfs_trace!(
            "vfs: read_grant_memory len={} target_base={:#x} target_space={}",
            len,
            target_base,
            target_space
        );
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let file_base = entry.base + entry.offset + offset;
        let page_offset = file_base & (PAGE_SIZE - 1);
        let page_start = file_base - page_offset;
        let total = page_offset + len;
        let pages = total.div_ceil(PAGE_SIZE);

        // Show first 8 bytes of source data for debugging
        let src_preview =
            unsafe { core::slice::from_raw_parts(file_base as *const u8, 8.min(len)) };
        vfs_trace!(
            "vfs: granting {} pages from {:#x}, file_base={:#x}, first bytes={:02x?}",
            pages,
            page_start,
            file_base,
            src_preview
        );

        for page_idx in 0..pages {
            let src = page_start + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                vfs_trace!("vfs: space_grant failed: {:?}", err);
                reply_msg.words[0] = err.to_errno() as usize;
                return Ok(());
            }
        }
        vfs_trace!("vfs: grant successful");

        reply_msg.words[0] = 0;
        reply_msg.words[1] = len;
        reply_msg.words[2] = page_offset;
        Ok(())
    }

    fn read_grant_remote(
        &self,
        entry: &fd_table::Ext2Entry,
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = entry.size.saturating_sub(offset);
        let len = requested.min(available).min(REMOTE_READ_CAP);
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let req = Message::new(
            FS_READ_GRANT,
            [0, 0, entry.inode as usize, offset, len, 0],
            5,
        );
        let mut reply = Message::new(0, [0; 6], 0);
        let mut payload = [0u8; TWO_USIZE_BYTES];
        payload[..USIZE_BYTES].copy_from_slice(&self.grant_buf_base.to_ne_bytes());
        payload[USIZE_BYTES..TWO_USIZE_BYTES]
            .copy_from_slice(&self.vfs_space_map_token.to_ne_bytes());

        let result = ipc::call_with_payload(entry.endpoint, &req, &payload, &mut reply);
        match result {
            Ok(()) => {
                let status = reply.words[0] as isize;
                if status < 0 {
                    reply_msg.words[0] = status as usize;
                    reply_msg.words[1] = 0;
                    reply_msg.words[2] = 0;
                    return Ok(());
                }

                let bytes_read = reply.words[1];
                let page_offset = reply.words[2];
                self.grant_buffer_to_caller(
                    bytes_read,
                    page_offset,
                    target_base,
                    target_space,
                    reply_msg,
                )
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                Ok(())
            }
        }
    }

    /// Read from cached data (Vec in memory).
    fn read_grant_cached(
        &self,
        data: &[u8],
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = data.len().saturating_sub(offset);
        let len = requested.min(available).min(REMOTE_READ_CAP);
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let slice = &data[offset..offset + len];
        self.grant_data_to_caller(slice, target_base, target_space, reply_msg)
    }

    fn read_grant_cached_region(
        &self,
        base: usize,
        len: usize,
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let data = unsafe { core::slice::from_raw_parts(base as *const u8, len) };
        self.read_grant_cached(
            data,
            offset,
            requested,
            target_base,
            target_space,
            reply_msg,
        )
    }

    fn cache_ext2_file(&mut self, entry: &fd_table::Ext2Entry) -> Option<CacheEntry> {
        let base = self.cache.reserve(entry.size)?;
        if self
            .read_remote_into_cache(entry.endpoint, entry.inode, entry.size, base)
            .is_err()
        {
            self.cache.release_reserved(base, entry.size);
            return None;
        }
        self.cache.insert(entry.inode, entry.size, base, entry.size);
        self.cache.get(entry.inode, entry.size)
    }

    /// Read entire file from remote backend into the cache region.
    fn read_remote_into_cache(
        &self,
        endpoint: usize,
        inode: u32,
        size: usize,
        target_base: usize,
    ) -> Result<()> {
        // #region agent log
        let _ = debug_print(&format!(
            "vfs: cache read_remote_start inode={} size={}",
            inode, size
        ));
        // #endregion
        let mut offset = 0;

        while offset < size {
            let remaining = size - offset;
            let chunk_size = remaining.min(REMOTE_READ_CAP);

            let req = Message::new(
                FS_READ_GRANT,
                [0, 0, inode as usize, offset, chunk_size, 0],
                5,
            );
            let mut reply = Message::new(0, [0; 6], 0);
            let mut payload = [0u8; TWO_USIZE_BYTES];
            payload[..USIZE_BYTES].copy_from_slice(&self.grant_buf_base.to_ne_bytes());
            payload[USIZE_BYTES..TWO_USIZE_BYTES]
                .copy_from_slice(&self.vfs_space_map_token.to_ne_bytes());

            ipc::call_with_payload(endpoint, &req, &payload, &mut reply)?;

            let status = reply.words[0] as isize;
            if status < 0 {
                return Err(Error::InvalidState);
            }

            let bytes_read = reply.words[1].min(chunk_size);
            let page_offset = reply.words[2];
            if page_offset >= self.grant_buf_size {
                return Err(Error::InvalidState);
            }
            let available = self.grant_buf_size - page_offset;
            if bytes_read > available {
                return Err(Error::InvalidState);
            }
            if bytes_read == 0 {
                return Err(Error::InvalidState);
            }

            let src = unsafe {
                core::slice::from_raw_parts(
                    (self.grant_buf_base + page_offset) as *const u8,
                    bytes_read,
                )
            };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src.as_ptr(),
                    (target_base + offset) as *mut u8,
                    bytes_read,
                );
            }
            offset += bytes_read;
        }

        // #region agent log
        let _ = debug_print(&format!(
            "vfs: cache read_remote_done inode={} size={}",
            inode, size
        ));
        // #endregion
        Ok(())
    }

    /// Chunked read from remote - used when file is too large to cache.
    fn read_grant_remote_chunked(
        &self,
        endpoint: usize,
        inode: u32,
        file_size: usize,
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = file_size.saturating_sub(offset);
        let len = requested.min(available).min(REMOTE_READ_CAP);
        // #region agent log
        let _ = debug_print(&format!(
            "vfs: remote_chunked len={} offset={} target={:#x}",
            len, offset, target_base
        ));
        // #endregion
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let req = Message::new(FS_READ_GRANT, [0, 0, inode as usize, offset, len, 0], 5);
        let mut reply = Message::new(0, [0; 6], 0);
        let mut payload = [0u8; TWO_USIZE_BYTES];
        payload[..USIZE_BYTES].copy_from_slice(&self.grant_buf_base.to_ne_bytes());
        payload[USIZE_BYTES..TWO_USIZE_BYTES]
            .copy_from_slice(&self.vfs_space_map_token.to_ne_bytes());

        // #region agent log
        let _ = debug_print("vfs: calling blkdev read_grant");
        // #endregion
        let result = ipc::call_with_payload(endpoint, &req, &payload, &mut reply);
        match result {
            Ok(()) => {
                let status = reply.words[0] as isize;
                // #region agent log
                let _ = debug_print(&format!(
                    "vfs: blkdev replied status={} bytes={}",
                    status, reply.words[1]
                ));
                // #endregion
                if status < 0 {
                    reply_msg.words[0] = status as usize;
                    reply_msg.words[1] = 0;
                    reply_msg.words[2] = 0;
                    return Ok(());
                }

                let bytes_read = reply.words[1];
                let page_offset = reply.words[2];
                self.grant_buffer_to_caller(
                    bytes_read,
                    page_offset,
                    target_base,
                    target_space,
                    reply_msg,
                )
            }
            Err(err) => {
                // #region agent log
                let _ = debug_print(&format!("vfs: blkdev call FAILED {:?}", err));
                // #endregion
                reply_msg.words[0] = err.to_errno() as usize;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                Ok(())
            }
        }
    }

    fn grant_buffer_to_caller(
        &self,
        len: usize,
        page_offset: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        // #region agent log
        let _ = debug_print(&format!(
            "vfs: grant_buffer len={} page_off={} target={:#x} space={}",
            len, page_offset, target_base, target_space
        ));
        // #endregion
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let total = page_offset + len;
        let pages = total.div_ceil(PAGE_SIZE);
        if pages * PAGE_SIZE > self.grant_buf_size {
            // #region agent log
            let _ = debug_print("vfs: buffer too small!");
            // #endregion
            reply_msg.words[0] = Error::BufferTooSmall.to_errno() as usize;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        // #region agent log
        let _ = debug_print(&format!(
            "vfs: granting {} pages from {:#x} to {:#x}",
            pages, self.grant_buf_base, target_base
        ));
        // #endregion
        for page_idx in 0..pages {
            let src = self.grant_buf_base + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                // #region agent log
                let _ = debug_print(&format!(
                    "vfs: space_grant FAILED page {} src={:#x} dst={:#x} err={:?}",
                    page_idx, src, dst, err
                ));
                // #endregion
                reply_msg.words[0] = err.to_errno() as usize;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                return Ok(());
            }
        }

        // #region agent log
        let _ = debug_print("vfs: grant_buffer OK");
        // #endregion
        reply_msg.words[0] = 0;
        reply_msg.words[1] = len;
        reply_msg.words[2] = page_offset;
        Ok(())
    }

    fn read_grant_virtual(
        &self,
        data: &[u8],
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = data.len().saturating_sub(offset);
        let len = requested.min(available).min(REMOTE_READ_CAP);
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let slice = &data[offset..offset + len];
        self.grant_data_to_caller(slice, target_base, target_space, reply_msg)
    }

    fn grant_data_to_caller(
        &self,
        data: &[u8],
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        vfs_trace!(
            "vfs: grant_data_to_caller start len={} target_base={:#x} target_space={}",
            data.len(),
            target_base,
            target_space
        );
        if data.is_empty() {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        if data.len() > self.grant_buf_size {
            vfs_trace!(
                "vfs: grant buffer too small len={} cap={}",
                data.len(),
                self.grant_buf_size
            );
            reply_msg.words[0] = Error::BufferTooSmall.to_errno() as usize;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        vfs_trace!(
            "vfs: grant_data len={} base={:#x} pages={}",
            data.len(),
            self.grant_buf_base,
            (data.len() + PAGE_SIZE - 1) / PAGE_SIZE
        );
        // Copy data to the buffer
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.grant_buf_base as *mut u8,
                data.len(),
            );
        }

        // Grant the pages to the caller
        let pages = (data.len() + PAGE_SIZE - 1) / PAGE_SIZE;
        for page_idx in 0..pages {
            let src = self.grant_buf_base + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                reply_msg.words[0] = err.to_errno() as usize;
                return Ok(());
            }
        }

        reply_msg.words[0] = 0;
        reply_msg.words[1] = data.len();
        reply_msg.words[2] = 0;
        vfs_trace!("vfs: grant_data_to_caller done");
        Ok(())
    }

    fn handle_readdir(&self, payload: &[u8], reply_token: usize) -> Result<()> {
        let mut reply_msg = Message::new(VFS_READDIR, [0; 6], 2);

        let path = match core::str::from_utf8(payload) {
            Ok(path) => path,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };

        vfs_trace!("vfs: readdir '{}'", path);

        // Use unified mount table for readdir
        match self.mounts.readdir(path) {
            Ok(entries) => {
                // Serialize entries: [name_len: u8, is_dir: u8, name bytes...]
                let mut data = Vec::new();
                for entry in &entries {
                    let name_bytes = entry.name.as_bytes();
                    if name_bytes.len() > 255 {
                        continue;
                    }
                    data.push(name_bytes.len() as u8);
                    data.push(if entry.is_dir { 1 } else { 0 });
                    data.extend_from_slice(name_bytes);
                }

                reply_msg.words[0] = 0;
                reply_msg.words[1] = entries.len();
                reply_with_payload(reply_token, &reply_msg, &data)
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
            }
        }
    }

    fn handle_map_elf(&mut self, msg: &Message, reply_token: usize) -> Result<()> {
        let client_id = msg.words[1];
        let fd = msg.words[2];
        let target_space = msg.words[3];
        let mut reply_msg = Message::new(VFS_MAP_ELF, [0; 6], 3);

        let Some(file) = self.files.get(client_id, fd).cloned() else {
            reply_msg.words[0] = Error::NotFound.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };

        match file {
            OpenFile::Ext2(entry) => {
                let cache_entry = if let Some(entry) = self.cache.get(entry.inode, entry.size) {
                    Some(entry)
                } else if entry.size <= FILE_CACHE_MAX_SIZE {
                    self.cache_ext2_file(&entry)
                } else {
                    None
                };

                let Some(cache_entry) = cache_entry else {
                    reply_msg.words[0] = Error::InvalidOperation.to_errno() as usize;
                    return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                };

                let data = unsafe {
                    core::slice::from_raw_parts(cache_entry.base as *const u8, cache_entry.len)
                };
                let elf = match ElfFile::parse(data) {
                    Ok(elf) => elf,
                    Err(_) => {
                        reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                        return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                };

                self.map_elf_segments(target_space, &elf, data)?;
                reply_msg.words[0] = 0;
                reply_msg.words[1] = elf.entry_point as usize;
                reply_msg.words[2] = cache_entry.len;
            }
            OpenFile::Memory(entry) => {
                let data = unsafe {
                    core::slice::from_raw_parts(
                        (entry.base + entry.offset) as *const u8,
                        entry.size,
                    )
                };
                let elf = match ElfFile::parse(data) {
                    Ok(elf) => elf,
                    Err(_) => {
                        reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                        return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                };
                self.map_elf_segments(target_space, &elf, data)?;
                reply_msg.words[0] = 0;
                reply_msg.words[1] = elf.entry_point as usize;
                reply_msg.words[2] = entry.size;
            }
            OpenFile::Virtual(_) => {
                reply_msg.words[0] = Error::InvalidOperation.to_errno() as usize;
            }
        }

        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn map_elf_segments(&self, target_space: usize, elf: &ElfFile, data: &[u8]) -> Result<()> {
        for segment in elf.segments_iter() {
            self.map_elf_segment(target_space, segment, data)?;
        }
        Ok(())
    }

    fn map_elf_segment(
        &self,
        target_space: usize,
        segment: &LoadableSegment,
        data: &[u8],
    ) -> Result<()> {
        let start = segment.vaddr as usize;
        if !start.is_multiple_of(PAGE_SIZE) {
            return Err(Error::InvalidArgument);
        }

        let mem_size = segment.mem_size as usize;
        if mem_size == 0 {
            return Ok(());
        }

        let file_offset = segment.file_offset as usize;
        let file_size = segment.file_size as usize;
        if file_offset + file_size > data.len() {
            return Err(Error::InvalidArgument);
        }

        let num_pages = mem_size.div_ceil(PAGE_SIZE);
        let data_ptr = data.as_ptr() as usize + file_offset;

        syscall::space_map_range(
            target_space,
            start,
            data_ptr,
            segment.page_flags() as usize,
            num_pages,
            file_size,
        )?;

        Ok(())
    }
}

fn parse_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    if buf.len() < size_of::<Message>() {
        return None;
    }
    let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
    let payload_len = msg.words[0];
    let header = size_of::<Message>();
    let end = header + payload_len;
    if end > buf.len() {
        return None;
    }
    Some((msg, &buf[header..end]))
}

fn parse_usize_pair(payload: &[u8]) -> Option<(usize, usize)> {
    if payload.len() < TWO_USIZE_BYTES {
        return None;
    }
    let mut bytes = [0u8; USIZE_BYTES];
    bytes.copy_from_slice(&payload[..USIZE_BYTES]);
    let first = usize::from_ne_bytes(bytes);
    bytes.copy_from_slice(&payload[USIZE_BYTES..TWO_USIZE_BYTES]);
    let second = usize::from_ne_bytes(bytes);
    Some((first, second))
}

fn map_grant_buffer(space_token: usize) -> Result<usize> {
    let pages = (GRANT_BUF_SIZE + PAGE_SIZE - 1) / PAGE_SIZE;
    if pages == 0 {
        return Err(Error::InvalidArgument);
    }

    match syscall::space_map_range(space_token, READ_BUF_BASE, 0, 0x03, pages, 0) {
        Ok(_) => {
            let _ = debug_print("vfs: grant buffer space_map_range ok");
            Ok(READ_BUF_BASE)
        }
        Err(Error::AlreadyExists) => {
            let _ = debug_print("vfs: grant buffer already mapped");
            Ok(READ_BUF_BASE)
        }
        Err(err) => {
            let _ = debug_print(&format!("vfs: grant buffer map failed {:?}", err));
            Err(err)
        }
    }
}

fn map_cache_buffer(space_token: usize) -> Result<usize> {
    let pages = (CACHE_BUF_SIZE + PAGE_SIZE - 1) / PAGE_SIZE;
    if pages == 0 {
        return Err(Error::InvalidArgument);
    }

    match syscall::space_map_range(space_token, CACHE_BUF_BASE, 0, 0x03, pages, 0) {
        Ok(_) => {
            let _ = debug_print("vfs: cache buffer space_map_range ok");
            Ok(CACHE_BUF_BASE)
        }
        Err(Error::AlreadyExists) => {
            let _ = debug_print("vfs: cache buffer already mapped");
            Ok(CACHE_BUF_BASE)
        }
        Err(err) => {
            let _ = debug_print(&format!("vfs: cache buffer map failed {:?}", err));
            Err(err)
        }
    }
}
