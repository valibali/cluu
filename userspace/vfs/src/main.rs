#![no_std]
#![no_main]

//! Virtual Filesystem Service for CLUU.
//!
//! Mount points are declared declaratively in `setup_mounts()`.
//! All path routing is handled by the unified MountTable.

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use core::mem::size_of;
use libcluu::fs::protocol::{VfsOp, VFS_CLOSE, VFS_OPEN, VFS_READ_GRANT, VFS_READDIR};
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
const USIZE_BYTES: usize = size_of::<usize>();
const TWO_USIZE_BYTES: usize = size_of::<usize>() * 2;

/// Buffer base for file data reads.
const READ_BUF_BASE: usize = 0x60000000;

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

    let mut server = VfsServer::new(endpoint, space_token, mounts);
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
            server.handle_message(&msg, payload)?;
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

    // Ext2 filesystem: forwarded to virtio-blk service
    // TODO: subscribe_output blocks forever if service doesn't exist
    // For now, skip blkdev mount - will be enabled when blkdev is properly available
    debug_print("vfs: skipping blkdev mount (service may not be available)")?;

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

struct VfsServer {
    endpoint: usize,
    space_token: usize,
    mounts: MountTable,
    files: FdTable,
}

impl VfsServer {
    fn new(endpoint: usize, space_token: usize, mounts: MountTable) -> Self {
        Self {
            endpoint,
            space_token,
            mounts,
            files: FdTable::new(),
        }
    }

    fn handle_message(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        let Some(op) = VfsOp::from_label(msg.tag.label) else {
            debug_print("vfs: unknown op")?;
            return Ok(());
        };
        let reply_token = extract_reply_token(msg).unwrap_or(self.endpoint);
        debug_print(&format!("vfs: handling {:?} reply_token={}", op, reply_token))?;
        let result = match op {
            VfsOp::Open => self.handle_open(msg, payload, reply_token),
            VfsOp::Close => self.handle_close(msg, reply_token),
            VfsOp::ReadGrant => self.handle_read_grant(msg, payload, reply_token),
            VfsOp::Readdir => self.handle_readdir(payload, reply_token),
        };
        debug_print(&format!("vfs: handled {:?} result={:?}", op, result))?;
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

        debug_print(&format!("vfs: open '{}' client={}", path, client_id))?;

        // Use unified mount table for all paths
        match self.mounts.open(path) {
            Ok(file) => {
                let size = file.size();
                let fd = self.files.open(client_id, file);
                reply_msg.words[0] = 0;
                reply_msg.words[1] = fd;
                reply_msg.words[2] = size;
            }
            Err(err) => {
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
        ipc::reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn handle_read_grant(&mut self, msg: &Message, payload: &[u8], reply_token: usize) -> Result<()> {
        let client_id = msg.words[1];
        let fd = msg.words[2];
        let offset = msg.words[3];
        let requested = msg.words[4];
        let mut reply_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);

        let Some((target_base, target_space)) = parse_usize_pair(payload) else {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        };

        let Some(file) = self.files.get(client_id, fd) else {
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

        match file {
            OpenFile::Memory(entry) => {
                self.read_grant_memory(entry, offset, requested, target_base, target_space, &mut reply_msg)?;
            }
            OpenFile::Ext2(entry) => {
                self.read_grant_remote(entry.inode, entry.size, offset, requested, target_base, target_space, &mut reply_msg)?;
            }
            OpenFile::Virtual(vfile) => {
                self.read_grant_virtual(&vfile.data, offset, requested, target_base, target_space, &mut reply_msg)?;
            }
        }

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
        let len = requested.min(available);
        debug_print(&format!("vfs: read_grant_memory len={} target_base={:#x} target_space={}", len, target_base, target_space))?;
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
        let src_preview = unsafe { core::slice::from_raw_parts(file_base as *const u8, 8.min(len)) };
        debug_print(&format!("vfs: granting {} pages from {:#x}, file_base={:#x}, first bytes={:02x?}",
            pages, page_start, file_base, src_preview))?;

        for page_idx in 0..pages {
            let src = page_start + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                debug_print(&format!("vfs: space_grant failed: {:?}", err))?;
                reply_msg.words[0] = err.to_errno() as usize;
                return Ok(());
            }
        }
        debug_print("vfs: grant successful")?;

        reply_msg.words[0] = 0;
        reply_msg.words[1] = len;
        reply_msg.words[2] = page_offset;
        Ok(())
    }

    fn read_grant_remote(
        &self,
        inode: u32,
        file_size: usize,
        offset: usize,
        requested: usize,
        target_base: usize,
        target_space: usize,
        reply_msg: &mut Message,
    ) -> Result<()> {
        let available = file_size.saturating_sub(offset);
        let len = requested.min(available);
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        // Create a temporary OpenFile for the read call
        let file = OpenFile::Ext2(fd_table::Ext2Entry {
            inode,
            size: file_size,
            data: None,
        });

        // Use mount table to read data
        let data = self.mounts.read("/mnt/disk", &file, offset, len)?;

        self.grant_data_to_caller(&data, target_base, target_space, reply_msg)
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
        let len = requested.min(available);
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
        if data.is_empty() {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        // Map pages for the read buffer and copy data
        let pages = (data.len() + PAGE_SIZE - 1) / PAGE_SIZE;
        let buf_base = READ_BUF_BASE;

        for page_idx in 0..pages {
            let virt = buf_base + page_idx * PAGE_SIZE;
            let _ = syscall::space_map(self.space_token, virt, 0, 0x03, 0);
        }

        // Copy data to the buffer
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), buf_base as *mut u8, data.len());
        }

        // Grant the pages to the caller
        for page_idx in 0..pages {
            let src = buf_base + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                reply_msg.words[0] = err.to_errno() as usize;
                return Ok(());
            }
        }

        reply_msg.words[0] = 0;
        reply_msg.words[1] = data.len();
        reply_msg.words[2] = 0;
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

        debug_print(&format!("vfs: readdir '{}'", path))?;

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
