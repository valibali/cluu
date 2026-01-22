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
/// Remote filesystem IPC label for zero-copy reads into the VFS grant buffer.
const FS_READ_GRANT: u32 = 0x306;
const USIZE_BYTES: usize = size_of::<usize>();
const TWO_USIZE_BYTES: usize = size_of::<usize>() * 2;

/// Buffer base for file data reads (shared grant window).
const READ_BUF_BASE: usize = 0x60000000;
/// Size of the shared grant window in the VFS address space.
const GRANT_BUF_SIZE: usize = 64 * 1024;
/// Cap for remote grant reads to avoid large transient allocations.
const REMOTE_READ_CAP: usize = GRANT_BUF_SIZE;
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
    let vfs_space_map_token =
        token_derive(space_token, Rights::SPACE_MAP.bits() as usize, u64::MAX)?;
    let mut server = VfsServer::new(
        endpoint,
        space_token,
        vfs_space_map_token,
        grant_buf_base,
        GRANT_BUF_SIZE,
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

struct VfsServer {
    endpoint: usize,
    space_token: usize,
    vfs_space_map_token: usize,
    grant_buf_base: usize,
    grant_buf_size: usize,
    mounts: MountTable,
    files: FdTable,
}

impl VfsServer {
    fn new(
        endpoint: usize,
        space_token: usize,
        vfs_space_map_token: usize,
        grant_buf_base: usize,
        grant_buf_size: usize,
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

        vfs_trace!("vfs: open '{}' client={}", path, client_id);

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
        if let Err(err) = ipc::reply(reply_token, &reply_msg, IpcFlags::empty()) {
            vfs_trace!("vfs: close reply failed {:?}", err);
        }
        Ok(())
    }

    fn handle_read_grant(&mut self, msg: &Message, payload: &[u8], reply_token: usize) -> Result<()> {
        let client_id = msg.words[1];
        let fd = msg.words[2];
        let offset = msg.words[3];
        let requested = msg.words[4];
        let mut reply_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);
        vfs_trace!(
            "vfs: read_grant start client={} fd={} off={} req={}",
            client_id, fd, offset, requested
        );

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
                self.read_grant_remote(entry, offset, requested, target_base, target_space, &mut reply_msg)?;
            }
            OpenFile::Virtual(vfile) => {
                self.read_grant_virtual(&vfile.data, offset, requested, target_base, target_space, &mut reply_msg)?;
            }
        }

        let res = ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
        let _ = debug_print("vfs: read_grant reply sent");
        res
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
                self.grant_buffer_to_caller(bytes_read, page_offset, target_base, target_space, reply_msg)
            }
            Err(err) => {
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
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        let total = page_offset + len;
        let pages = total.div_ceil(PAGE_SIZE);
        if pages * PAGE_SIZE > self.grant_buf_size {
            reply_msg.words[0] = Error::BufferTooSmall.to_errno() as usize;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return Ok(());
        }

        for page_idx in 0..pages {
            let src = self.grant_buf_base + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                reply_msg.words[0] = err.to_errno() as usize;
                reply_msg.words[1] = 0;
                reply_msg.words[2] = 0;
                return Ok(());
            }
        }

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
