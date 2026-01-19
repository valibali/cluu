#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use core::mem::size_of;
use libcluu::fs::protocol::{VfsOp, VFS_CLOSE, VFS_OPEN, VFS_READ_GRANT};
use libcluu::ipc::extract_reply_token;
use libcluu::*;

mod fd_table;
mod mount;

use fd_table::{FdTable, FileEntry};
use mount::MountTable;

const SVC_TOKEN_LISTEN: usize = 7;
const IPC_MESSAGE_MAX: usize = 256;
const USIZE_BYTES: usize = size_of::<usize>();
const TWO_USIZE_BYTES: usize = size_of::<usize>() * 2;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if let Err(err) = run_vfs() {
        let _ = debug_print(&format!("vfs: fatal error {:?}", err));
        return 1;
    }
    0
}

fn run_vfs() -> Result<()> {
    let info = process_info();
    let endpoint = info.tokens[SVC_TOKEN_LISTEN];
    let space_token = info.tokens[TOKEN_SPACE];
    let initrd_size = info.params[PARAM_INITRD_SIZE] as usize;
    let initrd = map_initrd_slice(initrd_size);

    registry::init("vfs")?;
    registry::register_default_outputs()?;
    registry::register_output("main", endpoint)?;

    let mut server = VfsServer::new(endpoint, space_token, initrd);
    let registry_endpoint = registry::control_endpoint();
    let mut buf = [0u8; IPC_MESSAGE_MAX];
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

fn map_initrd_slice(initrd_size: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, initrd_size) }
}

struct VfsServer<'a> {
    endpoint: usize,
    space_token: usize,
    mounts: MountTable<'a>,
    files: FdTable,
}

impl<'a> VfsServer<'a> {
    fn new(endpoint: usize, space_token: usize, initrd: &'a [u8]) -> Self {
        Self {
            endpoint,
            space_token,
            mounts: MountTable::new(initrd),
            files: FdTable::new(),
        }
    }

    fn handle_message(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        let Some(op) = VfsOp::from_label(msg.tag.label) else {
            return Ok(());
        };
        // Extract reply token for call messages
        let reply_token = extract_reply_token(msg).unwrap_or(self.endpoint);
        match op {
            VfsOp::Open => self.handle_open(msg, payload, reply_token),
            VfsOp::Close => self.handle_close(msg, reply_token),
            VfsOp::ReadGrant => self.handle_read_grant(msg, payload, reply_token),
        }
    }

    fn handle_open(&mut self, msg: &Message, payload: &[u8], reply_token: usize) -> Result<()> {
        let client_id = msg.words[1];
        let mut reply_msg = Message::new(VFS_OPEN, [0; 6], 3);
        let path = match core::str::from_utf8(payload) {
            Ok(path) => path,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        match self.open_path(path) {
            Ok(entry) => {
                let fd = self.files.open(client_id, entry);
                reply_msg.words[0] = 0;
                reply_msg.words[1] = fd;
                reply_msg.words[2] = entry.size;
            }
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
            }
        }
        reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn handle_close(&mut self, msg: &Message, reply_token: usize) -> Result<()> {
        let client_id = msg.words[1];
        let fd = msg.words[2];
        self.files.close(client_id, fd);
        let mut reply_msg = Message::new(VFS_CLOSE, [0; 6], 1);
        reply_msg.words[0] = 0;
        reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn handle_read_grant(&mut self, msg: &Message, payload: &[u8], reply_token: usize) -> Result<()> {
        let client_id = msg.words[1];
        let fd = msg.words[2];
        let offset = msg.words[3];
        let requested = msg.words[4];
        let mut reply_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);
        let Some((target_base, target_space)) = parse_usize_pair(payload) else {
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            return reply(reply_token, &reply_msg, IpcFlags::empty());
        };
        let Some(entry) = self.files.get(client_id, fd) else {
            reply_msg.words[0] = Error::NotFound.to_errno() as usize;
            return reply(reply_token, &reply_msg, IpcFlags::empty());
        };
        if requested == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return reply(reply_token, &reply_msg, IpcFlags::empty());
        }
        if target_base & (PAGE_SIZE - 1) != 0 {
            reply_msg.words[0] = Error::InvalidArgument as isize as usize;
            return reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        let available = entry.size.saturating_sub(offset);
        let len = requested.min(available);
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        let file_base = entry.base + entry.offset + offset;
        let page_offset = file_base & (PAGE_SIZE - 1);
        let page_start = file_base - page_offset;
        let total = page_offset + len;
        let pages = total.div_ceil(PAGE_SIZE);

        for page_idx in 0..pages {
            let src = page_start + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            if let Err(err) = space_grant(self.space_token, target_space, src, dst, 0) {
                reply_msg.words[0] = err.to_errno() as usize;
                return reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        }

        reply_msg.words[0] = 0;
        reply_msg.words[1] = len;
        reply_msg.words[2] = page_offset;
        reply(reply_token, &reply_msg, IpcFlags::empty())
    }

    fn open_path(&mut self, path: &str) -> Result<FileEntry> {
        self.mounts.open(path)
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

fn parse_usize_payload(payload: &[u8]) -> Option<usize> {
    if payload.len() < USIZE_BYTES {
        return None;
    }
    let mut bytes = [0u8; USIZE_BYTES];
    bytes.copy_from_slice(&payload[..USIZE_BYTES]);
    Some(usize::from_ne_bytes(bytes))
}

fn parse_usize_pair(payload: &[u8]) -> Option<(usize, usize)> {
    if payload.len() < TWO_USIZE_BYTES {
        return None;
    }
    let first = parse_usize_payload(payload)?;
    let mut bytes = [0u8; USIZE_BYTES];
    bytes.copy_from_slice(&payload[USIZE_BYTES..TWO_USIZE_BYTES]);
    let second = usize::from_ne_bytes(bytes);
    Some((first, second))
}
