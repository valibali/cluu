#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use core::mem::size_of;
use libcluu::fs::protocol::{VfsOp, VFS_CLOSE, VFS_OPEN, VFS_READ_GRANT};
use libcluu::tar::find_member;
use libcluu::*;

const SVC_TOKEN_LISTEN: usize = 7;
const IPC_MESSAGE_MAX: usize = 256;

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
    debug_print("vfs: ready")?;

    let mut server = VfsServer::new(endpoint, space_token, initrd);
    let mut buf = [0u8; IPC_MESSAGE_MAX];
    loop {
        let len = ipc_recv_timeout(endpoint, &mut buf, u64::MAX)?;
        if let Some((msg, payload)) = parse_message(&buf[..len]) {
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
    initrd: &'a [u8],
    files: FileTable,
}

impl<'a> VfsServer<'a> {
    fn new(endpoint: usize, space_token: usize, initrd: &'a [u8]) -> Self {
        Self {
            endpoint,
            space_token,
            initrd,
            files: FileTable::new(),
        }
    }

    fn handle_message(&mut self, msg: &Message, payload: &[u8]) -> Result<()> {
        let Some(op) = VfsOp::from_label(msg.tag.label) else {
            return Ok(());
        };
        match op {
            VfsOp::Open => self.handle_open(msg, payload),
            VfsOp::Close => self.handle_close(msg),
            VfsOp::ReadGrant => self.handle_read_grant(msg),
        }
    }

    fn handle_open(&mut self, _msg: &Message, payload: &[u8]) -> Result<()> {
        let path = core::str::from_utf8(payload).map_err(|_| Error::InvalidArgument)?;
        let entry = self.open_path(path)?;
        let mut reply_msg = Message::new(VFS_OPEN, [0; 6], 3);
        reply_msg.words[0] = 0;
        reply_msg.words[1] = entry.fd;
        reply_msg.words[2] = entry.size;
        reply(self.endpoint, &reply_msg, IpcFlags::empty())
    }

    fn handle_close(&mut self, msg: &Message) -> Result<()> {
        let fd = msg.words[1];
        self.files.close(fd);
        let mut reply_msg = Message::new(VFS_CLOSE, [0; 6], 1);
        reply_msg.words[0] = 0;
        reply(self.endpoint, &reply_msg, IpcFlags::empty())
    }

    fn handle_read_grant(&mut self, msg: &Message) -> Result<()> {
        let fd = msg.words[1];
        let offset = msg.words[2];
        let requested = msg.words[3];
        let target_space = msg.words[4];
        let target_base = msg.words[5];

        let mut reply_msg = Message::new(VFS_READ_GRANT, [0; 6], 3);
        let Some(entry) = self.files.get(fd) else {
            reply_msg.words[0] = Error::NotFound as isize as usize;
            return reply(self.endpoint, &reply_msg, IpcFlags::empty());
        };
        if requested == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return reply(self.endpoint, &reply_msg, IpcFlags::empty());
        }
        if target_base & (PAGE_SIZE - 1) != 0 {
            reply_msg.words[0] = Error::InvalidArgument as isize as usize;
            return reply(self.endpoint, &reply_msg, IpcFlags::empty());
        }

        let available = entry.size.saturating_sub(offset);
        let len = requested.min(available);
        if len == 0 {
            reply_msg.words[0] = 0;
            reply_msg.words[1] = 0;
            reply_msg.words[2] = 0;
            return reply(self.endpoint, &reply_msg, IpcFlags::empty());
        }

        let file_base = self.initrd.as_ptr() as usize + entry.offset + offset;
        let page_offset = file_base & (PAGE_SIZE - 1);
        let page_start = file_base - page_offset;
        let total = page_offset + len;
        let pages = total.div_ceil(PAGE_SIZE);

        for page_idx in 0..pages {
            let src = page_start + page_idx * PAGE_SIZE;
            let dst = target_base + page_idx * PAGE_SIZE;
            space_grant(self.space_token, target_space, src, dst, 0)?;
        }

        reply_msg.words[0] = 0;
        reply_msg.words[1] = len;
        reply_msg.words[2] = page_offset;
        reply(self.endpoint, &reply_msg, IpcFlags::empty())
    }

    fn open_path(&mut self, path: &str) -> Result<VfsFile> {
        let slice = find_member(self.initrd, path).ok_or(Error::NotFound)?;
        let base = self.initrd.as_ptr() as usize;
        let offset = slice.as_ptr() as usize - base;
        Ok(self.files.open(offset, slice.len()))
    }
}

#[derive(Clone, Copy)]
struct VfsFile {
    fd: usize,
    offset: usize,
    size: usize,
}

struct FileTable {
    next_fd: usize,
    entries: BTreeMap<usize, VfsFile>,
}

impl FileTable {
    fn new() -> Self {
        Self {
            next_fd: 4,
            entries: BTreeMap::new(),
        }
    }

    fn open(&mut self, offset: usize, size: usize) -> VfsFile {
        let fd = self.next_fd;
        self.next_fd += 1;
        let entry = VfsFile { fd, offset, size };
        self.entries.insert(fd, entry);
        entry
    }

    fn get(&self, fd: usize) -> Option<VfsFile> {
        self.entries.get(&fd).copied()
    }

    fn close(&mut self, fd: usize) {
        self.entries.remove(&fd);
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
