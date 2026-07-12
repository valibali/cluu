#![no_std]
#![no_main]

//! Virtio-9p host folder mount service.
//!
//! Discovers a virtio-9p-pci device, speaks 9P2000.L over the request
//! virtqueue, and serves FS_* IPC labels (same protocol virtio-blk serves)
//! so VFS can mount it as a remote backend at /host.

extern crate alloc;

mod protocol;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

use cluu_virtio_core::dma::{DmaPool, DmaRegion};
use cluu_virtio_core::pci;
use cluu_virtio_core::transport::{FeatureBits, ModernPciTransport, Transport};
use cluu_virtio_core::virtqueue::{Virtqueue, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};

use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2, TOKEN_IPC, TOKEN_SPACE};
use libcluu::ipc::{extract_reply_id, reply, reply_with_payload};
use libcluu::registry;
use libcluu::syscall::{endpoint_create, ipc_recv_any_with_sender, pci_config_read, space_map_range, virt_to_phys};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, space_grant, Error, Result, PAGE_SIZE};

use protocol::*;

const FS_OPEN: u32 = 0x300;
const FS_CLOSE: u32 = 0x301;
const FS_READ: u32 = 0x302;
const FS_READ_GRANT: u32 = 0x306;
const FS_STAT: u32 = 0x303;
const FS_READDIR: u32 = 0x304;
const FS_WRITE: u32 = 0x305;
const FS_UNLINK: u32 = 0x307;
const FS_MKDIR: u32 = 0x308;
const FS_RMDIR: u32 = 0x309;
const FS_RENAME: u32 = 0x30A;
const FS_CREATE: u32 = 0x30B;
const FS_REALPATH: u32 = 0x30D;

const IPC_MESSAGE_MAX: usize = 256;

const DMA_POOL_VA: usize = 0x5300_0000;
const DMA_POOL_PAGES: usize = 64;
const MMIO_VA_BASE: usize = 0x5400_0000;

const MSIZE: usize = 8 * 1024;
const REQ_BUF_VA: usize = 0x5500_0000;
const RESP_BUF_VA: usize = 0x5520_0000;
const BUF_PAGES: usize = 2;

const GRANT_SCRATCH_BASE: usize = 0x5600_0000;
const GRANT_SCRATCH_SIZE: usize = 4 * 1024 * 1024;

const ROOT_FID: u32 = 0;
const FIRST_FID: u32 = 1;

const QTDIR: u8 = 0x80;
const GETATTR_ALL: u64 = 0x3FFF;

const RLERROR: u8 = 7;
const TLOPEN: u8 = 12;
const RLOPEN: u8 = 13;
const TGETATTR: u8 = 24;
const RGETATTR: u8 = 25;
const TREADDIR: u8 = 40;
const RREADDIR: u8 = 41;
const TLCREATE: u8 = 66;
const RLCREATE: u8 = 67;
const TMKDIR: u8 = 72;
const RMKDIR: u8 = 73;
const TUNLINKAT: u8 = 76;
const RUNLINKAT: u8 = 77;
const TVERSION: u8 = 100;
const RVERSION: u8 = 101;
const TATTACH: u8 = 104;
const RATTACH: u8 = 105;
const TWALK: u8 = 110;
const RWALK: u8 = 111;
const TREAD: u8 = 116;
const RREAD: u8 = 117;
const TWRITE: u8 = 118;
const RWRITE: u8 = 119;
const TCLUNK: u8 = 120;
const RCLUNK: u8 = 121;

struct NinepClient {
    transport: ModernPciTransport,
    vq: Virtqueue,
    req_region: DmaRegion,
    resp_region: DmaRegion,
    next_fid: u32,
    msize: u32,
}

impl NinepClient {
    fn new(
        mut transport: ModernPciTransport,
        mut pool: DmaPool,
        space_token: usize,
    ) -> Result<Self> {
        let vq = Virtqueue::new(&mut pool, 64)?;
        transport.configure_queue(0, &vq)?;
        transport.set_driver_ok()?;

        space_map_range(space_token, REQ_BUF_VA, 0, 0x03, BUF_PAGES, 0)?;
        space_map_range(space_token, RESP_BUF_VA, 0, 0x03, BUF_PAGES, 0)?;

        let req_phys = virt_to_phys(space_token, REQ_BUF_VA)? as u64;
        let resp_phys = virt_to_phys(space_token, RESP_BUF_VA)? as u64;

        Ok(Self {
            transport,
            vq,
            req_region: DmaRegion { virt: REQ_BUF_VA, phys: req_phys, len: MSIZE },
            resp_region: DmaRegion { virt: RESP_BUF_VA, phys: resp_phys, len: MSIZE },
            next_fid: FIRST_FID,
            msize: MSIZE as u32,
        })
    }

    fn alloc_fid(&mut self) -> u32 {
        let f = self.next_fid;
        self.next_fid += 1;
        f
    }

    fn round_trip(&mut self, req_len: usize) -> Result<&'static [u8]> {
        if req_len > MSIZE {
            return Err(Error::BufferTooSmall);
        }

        let chain = self.vq.alloc_chain(2).ok_or(Error::Busy)?;
        let descs = self.collect_chain(chain.head, 2);

        self.vq.desc_set(
            descs[0],
            self.req_region.phys,
            req_len as u32,
            VRING_DESC_F_NEXT,
            descs[1],
        );
        self.vq.desc_set(
            descs[1],
            self.resp_region.phys,
            MSIZE as u32,
            VRING_DESC_F_WRITE,
            0,
        );

        self.vq.submit(chain, 1);
        self.transport.notify(0);

        let mut spins = 0u64;
        loop {
            let _ = self.transport.isr_status();
            if let Some((_cookie, resp_len)) = self.vq.pop_used() {
                let len = resp_len as usize;
                if len > MSIZE {
                    return Err(Error::InvalidState);
                }
                return Ok(unsafe {
                    core::slice::from_raw_parts(self.resp_region.virt as *const u8, len)
                });
            }
            spins += 1;
            if spins % 1024 == 0 {
                let _ = libcluu::syscall::yield_cpu();
            }
            core::hint::spin_loop();
        }
    }

    fn collect_chain(&self, head: u16, n: u16) -> Vec<u16> {
        let mut out = Vec::with_capacity(n as usize);
        let mut cur = head;
        for _ in 0..n {
            out.push(cur);
            let next = unsafe {
                let p = (self.vq.desc_region.virt
                    as *const cluu_virtio_core::virtqueue::VRingDesc)
                    .add(cur as usize);
                (*p).next
            };
            cur = next;
        }
        out
    }

    fn build_request(&mut self, msg_type: u8, tag: u16, body: &dyn Fn(&mut Encoder)) -> Result<usize> {
        let buf = unsafe { core::slice::from_raw_parts_mut(self.req_region.virt as *mut u8, MSIZE) };
        let mut enc = Encoder::new(&mut buf[4..]);
        if !enc.put_u8(msg_type) { return Err(Error::BufferTooSmall); }
        if !enc.put_u16(tag) { return Err(Error::BufferTooSmall); }
        body(&mut enc);
        let body_len = enc.bytes_written();
        let total = body_len + 4;
        buf[0..4].copy_from_slice(&(total as u32).to_le_bytes());
        Ok(total)
    }

    fn parse_header(resp: &[u8]) -> Result<(u8, u16)> {
        if resp.len() < 7 {
            return Err(Error::InvalidState);
        }
        let size = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]) as usize;
        if size > resp.len() {
            return Err(Error::InvalidState);
        }
        let msg_type = resp[4];
        let tag = u16::from_le_bytes([resp[5], resp[6]]);
        Ok((msg_type, tag))
    }

    fn version(&mut self) -> Result<u32> {
        let msize = self.msize;
        let req_len = self.build_request(TVERSION, 0xFFFF, &|enc| {
            enc.put_u32(msize);
            enc.put_string("9P2000.L");
        })?;
        let resp = self.round_trip(req_len)?;
        Self::parse_header(resp)?;
        let mut dec = Decoder::new(&resp[7..]);
        let msize = dec.get_u32().ok_or(Error::InvalidState)?;
        let _version = dec.get_string();
        self.msize = msize;
        Ok(msize)
    }

    fn attach(&mut self) -> Result<()> {
        let req_len = self.build_request(TATTACH, 1, &|enc| {
            enc.put_u32(ROOT_FID);
            enc.put_u32(0xFFFFFFFF);
            enc.put_string("nobody");
            enc.put_string("/");
            enc.put_u32(0);
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::InvalidState);
        }
        let mut dec = Decoder::new(&resp[7..]);
        let _qid = dec.get_qid().ok_or(Error::InvalidState)?;
        self.next_fid = FIRST_FID;
        Ok(())
    }

    fn walk(&mut self, from_fid: u32, new_fid: u32, components: &[&str]) -> Result<()> {
        let req_len = self.build_request(TWALK, 1, &|enc| {
            enc.put_u32(from_fid);
            enc.put_u32(new_fid);
            enc.put_u16(components.len() as u16);
            for c in components {
                enc.put_string(c);
            }
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::NotFound);
        }
        let mut dec = Decoder::new(&resp[7..]);
        let nwalked = dec.get_u16().ok_or(Error::InvalidState)?;
        if nwalked as usize != components.len() {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    fn lopen(&mut self, fid: u32, flags: u32) -> Result<()> {
        let req_len = self.build_request(TLOPEN, 1, &|enc| {
            enc.put_u32(fid);
            enc.put_u32(flags);
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    fn read(&mut self, fid: u32, offset: u64, len: u32) -> Result<Vec<u8>> {
        let req_len = self.build_request(TREAD, 1, &|enc| {
            enc.put_u32(fid);
            enc.put_u64(offset);
            enc.put_u32(len);
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::InvalidState);
        }
        let mut dec = Decoder::new(&resp[7..]);
        let count = dec.get_u32().ok_or(Error::InvalidState)?;
        let data = dec.get_bytes(count as usize).ok_or(Error::InvalidState)?;
        Ok(data.to_vec())
    }

    fn readdir(&mut self, fid: u32, offset: u64, len: u32) -> Result<Vec<u8>> {
        let req_len = self.build_request(TREADDIR, 1, &|enc| {
            enc.put_u32(fid);
            enc.put_u64(offset);
            enc.put_u32(len);
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::InvalidState);
        }
        let mut dec = Decoder::new(&resp[7..]);
        let count = dec.get_u32().ok_or(Error::InvalidState)?;
        let data = dec.get_bytes(count as usize).ok_or(Error::InvalidState)?;
        Ok(data.to_vec())
    }

    fn getattr(&mut self, fid: u32) -> Result<GetAttr> {
        let req_len = self.build_request(TGETATTR, 1, &|enc| {
            enc.put_u32(fid);
            enc.put_u64(GETATTR_ALL);
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::InvalidState);
        }
        let mut dec = Decoder::new(&resp[7..]);
        let _valid = dec.get_u64().ok_or(Error::InvalidState)?;
        let _qid = dec.get_qid().ok_or(Error::InvalidState)?;
        let mode = dec.get_u32().ok_or(Error::InvalidState)?;
        let uid = dec.get_u32().ok_or(Error::InvalidState)?;
        let gid = dec.get_u32().ok_or(Error::InvalidState)?;
        let nlink = dec.get_u64().ok_or(Error::InvalidState)?;
        let _rdev = dec.get_u64().ok_or(Error::InvalidState)?;
        let size = dec.get_u64().ok_or(Error::InvalidState)?;
        let _blksize = dec.get_u64().ok_or(Error::InvalidState)?;
        let _blocks = dec.get_u64().ok_or(Error::InvalidState)?;
        let _atime_sec = dec.get_u64().ok_or(Error::InvalidState)?;
        let _atime_nsec = dec.get_u64().ok_or(Error::InvalidState)?;
        let mtime_sec = dec.get_u64().ok_or(Error::InvalidState)?;
        let _mtime_nsec = dec.get_u64().ok_or(Error::InvalidState)?;
        Ok(GetAttr { mode, uid, gid, nlink, size, mtime: mtime_sec })
    }

    fn clunk(&mut self, fid: u32) -> Result<()> {
        let req_len = self.build_request(TCLUNK, 1, &|enc| {
            enc.put_u32(fid);
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::InvalidState);
        }
        Ok(())
    }

    fn write(&mut self, fid: u32, offset: u64, data: &[u8]) -> Result<u32> {
        let req_len = self.build_request(TWRITE, 1, &|enc| {
            enc.put_u32(fid);
            enc.put_u64(offset);
            enc.put_u32(data.len() as u32);
            enc.put_bytes(data);
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::InvalidState);
        }
        let mut dec = Decoder::new(&resp[7..]);
        let count = dec.get_u32().ok_or(Error::InvalidState)?;
        Ok(count)
    }

    fn mkdir(&mut self, dir_fid: u32, name: &str, mode: u32, gid: u32) -> Result<()> {
        let req_len = self.build_request(TMKDIR, 1, &|enc| {
            enc.put_u32(dir_fid);
            enc.put_string(name);
            enc.put_u32(mode);
            enc.put_u32(gid);
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::InvalidState);
        }
        Ok(())
    }

    fn unlinkat(&mut self, dir_fid: u32, name: &str, flags: u32) -> Result<()> {
        let req_len = self.build_request(TUNLINKAT, 1, &|enc| {
            enc.put_u32(dir_fid);
            enc.put_string(name);
            enc.put_u32(flags);
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::InvalidState);
        }
        Ok(())
    }

    fn lcreate(&mut self, dir_fid: u32, name: &str, flags: u32, mode: u32, gid: u32) -> Result<()> {
        let req_len = self.build_request(TLCREATE, 1, &|enc| {
            enc.put_u32(dir_fid);
            enc.put_string(name);
            enc.put_u32(flags);
            enc.put_u32(mode);
            enc.put_u32(gid);
        })?;
        let resp = self.round_trip(req_len)?;
        let (msg_type, _tag) = Self::parse_header(resp)?;
        if msg_type == RLERROR {
            return Err(Error::InvalidState);
        }
        Ok(())
    }

    fn walk_path(&mut self, path: &str) -> Result<u32> {
        let new_fid = self.alloc_fid();
        let components: Vec<&str> = if path.is_empty() {
            Vec::new()
        } else {
            path.split('/').filter(|s| !s.is_empty()).collect()
        };
        match self.walk(ROOT_FID, new_fid, &components) {
            Ok(()) => Ok(new_fid),
            Err(e) => {
                if self.next_fid > FIRST_FID {
                    self.next_fid -= 1;
                }
                Err(e)
            }
        }
    }

    fn walk_parent(&mut self, path: &str) -> Result<(u32, String)> {
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if components.is_empty() {
            return Err(Error::InvalidArgument);
        }
        let (parent_comps, basename) = components.split_at(components.len() - 1);
        let parent_fid = self.alloc_fid();
        match self.walk(ROOT_FID, parent_fid, parent_comps) {
            Ok(()) => Ok((parent_fid, String::from(basename[0]))),
            Err(e) => {
                if self.next_fid > FIRST_FID {
                    self.next_fid -= 1;
                }
                Err(e)
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&format!("virtio-9p: error {:?}", e));
            -1
        }
    }
}

fn run() -> Result<()> {
    debug_print("virtio-9p: starting")?;

    let info = process_info();
    let pci_token = info.tokens[TOKEN_EXTRA_1];
    let space_token = info.tokens[TOKEN_SPACE];
    let ipc_token = info.tokens[TOKEN_IPC];

    let pci_device = match pci::find_virtio_device(pci_token, &[0x1049], &[0x1049]) {
        Ok(d) => {
            debug_print(&format!("virtio-9p: found PCI device {:?}", d))?;
            d
        }
        Err(e) => {
            debug_print(&format!("virtio-9p: find_virtio_device failed: {:?}", e))?;
            return Err(e);
        }
    };

    pci::enable_device(pci_token, &pci_device)?;

    let pool = DmaPool::new(space_token, DMA_POOL_VA, DMA_POOL_PAGES)?;

    let grant_scratch_pages = GRANT_SCRATCH_SIZE.div_ceil(PAGE_SIZE);
    match space_map_range(space_token, GRANT_SCRATCH_BASE, 0, 0x03, grant_scratch_pages, 0) {
        Ok(_) | Err(Error::AlreadyExists) => {}
        Err(e) => return Err(e),
    }

    let bar_phys = pci_device.cap_bar_phys;
    let bar_size = pci_device.cap_bar_size;
    let mut transport = ModernPciTransport::new(
        space_token,
        pci_device.clone(),
        bar_phys,
        bar_size,
        MMIO_VA_BASE,
    )?;

    transport.reset()?;
    let dev_feats = transport.read_device_features()?;
    let want = FeatureBits::VERSION_1.bits() & dev_feats;
    transport.write_driver_features(want)?;

    let cfg_va = transport.device_cfg_va;
    let mount_tag: String = unsafe {
        let tag_len = core::ptr::read_volatile(cfg_va as *const u16) as usize;
        let max = tag_len.min(255);
        let mut buf = [0u8; 256];
        for i in 0..max {
            buf[i] = core::ptr::read_volatile((cfg_va + 2 + i) as *const u8);
        }
        String::from(core::str::from_utf8(&buf[..max]).unwrap_or("<invalid>"))
    };
    debug_print(&format!("virtio-9p: mount_tag={:?}", mount_tag))?;

    let intr_line_word = pci_config_read(
        pci_token,
        pci_device.bus,
        pci_device.device,
        pci_device.function,
        0x3c,
    )?;
    let irq_number = (intr_line_word & 0xFF) as usize;
    debug_print(&format!(
        "virtio-9p: PCI Interrupt Line = {} (raw 0x{:08x})",
        irq_number, intr_line_word
    ))?;

    let irq_token = info.tokens[TOKEN_EXTRA_2];
    let irq = cluu_virtio_core::IrqSource::new(ipc_token, irq_token, irq_number)?;
    debug_print(&format!(
        "virtio-9p: IRQ attached (endpoint={} irq={})",
        irq.endpoint, irq.irq_number
    ))?;

    let mut client = NinepClient::new(transport, pool, space_token)?;

    let msize = client.version()?;
    debug_print(&format!("virtio-9p: version negotiated, msize={}", msize))?;
    client.attach()?;
    debug_print("virtio-9p: attached to root")?;

    registry::init("hostfs")?;
    let listen_endpoint = info.tokens[TOKEN_EXTRA_0];
    let listen_endpoint = if listen_endpoint != 0 {
        listen_endpoint
    } else {
        endpoint_create(ipc_token)?
    };
    registry::register_output("main", listen_endpoint)?;
    debug_print("virtio-9p: registered as hostfs:main")?;

    let registry_endpoint = registry::control_endpoint();

    let mut buf = [0u8; 4096];
    loop {
        let tokens = [listen_endpoint, irq.endpoint, registry_endpoint];
        let (idx, len, _sender_tid) = match ipc_recv_any_with_sender(&tokens, &mut buf, u64::MAX) {
            Ok(t) => t,
            Err(_) => continue,
        };

        if idx == 1 {
            let _ = client.transport.isr_status();
            continue;
        }

        if len < core::mem::size_of::<Message>() {
            continue;
        }
        let msg = unsafe { &*(buf.as_ptr() as *const Message) };
        let payload = &buf[core::mem::size_of::<Message>()..len];

        if idx == 2 {
            let _ = registry::handle_incoming_message(msg, payload);
            continue;
        }

        handle_fs_request(&mut client, space_token, msg, payload);
    }
}

fn handle_fs_request(client: &mut NinepClient, space_token: usize, msg: &Message, payload: &[u8]) {
    let reply_token = extract_reply_id(msg);

    match msg.tag.label {
        FS_OPEN => {
            let path = core::str::from_utf8(payload).unwrap_or("");
            match open_path(client, path) {
                Ok((fid, size, is_dir)) => {
                    let flags = if is_dir { 1 } else { 0 };
                    let reply_msg = Message::new(
                        FS_OPEN,
                        [0, fid as usize, size as usize, flags, 0, 0],
                        4,
                    );
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(_) => send_error_reply(reply_token, -3),
            }
        }

        FS_READ => {
            let fid = msg.words[2] as u32;
            let offset = msg.words[3] as u64;
            let len = msg.words[4].min(IPC_MESSAGE_MAX - core::mem::size_of::<Message>());

            match client.read(fid, offset, len as u32) {
                Ok(data) => {
                    let bytes_read = data.len();
                    let reply_msg = Message::new(FS_READ, [0, 0, bytes_read, 0, 0, 0], 3);
                    if let Some(token) = reply_token {
                        let _ = reply_with_payload(token, &reply_msg, &data);
                    }
                }
                Err(_) => send_error_reply_shifted(reply_token, -1),
            }
        }

        FS_READ_GRANT => {
            let fid = msg.words[2] as u32;
            let offset = msg.words[3] as u64;
            let len = msg.words[4];

            let Some((target_base, target_space)) = parse_usize_pair(payload) else {
                send_error_reply(reply_token, -2);
                return;
            };

            if len == 0 {
                let reply_msg = Message::new(FS_READ_GRANT, [0, 0, 0, 0, 0, 0], 2);
                if let Some(token) = reply_token {
                    let _ = reply(token, &reply_msg, IpcFlags::empty());
                }
                return;
            }

            if len > GRANT_SCRATCH_SIZE {
                send_error_reply(reply_token, -4);
                return;
            }

            match client.read(fid, offset, len as u32) {
                Ok(data) => {
                    let bytes_read = data.len();
                    if bytes_read == 0 {
                        let reply_msg = Message::new(FS_READ_GRANT, [0, 0, 0, 0, 0, 0], 2);
                        if let Some(token) = reply_token {
                            let _ = reply(token, &reply_msg, IpcFlags::empty());
                        }
                        return;
                    }

                    let scratch = unsafe {
                        core::slice::from_raw_parts_mut(GRANT_SCRATCH_BASE as *mut u8, GRANT_SCRATCH_SIZE)
                    };
                    scratch[..bytes_read].copy_from_slice(&data);

                    let pages = bytes_read.div_ceil(PAGE_SIZE);
                    let mut grant_err = None;
                    for page_idx in 0..pages {
                        let src = GRANT_SCRATCH_BASE + page_idx * PAGE_SIZE;
                        let dst = target_base + page_idx * PAGE_SIZE;
                        if let Err(err) = space_grant(space_token, target_space, src, dst, 0x02) {
                            grant_err = Some(err);
                            break;
                        }
                    }

                    if grant_err.is_some() {
                        send_error_reply(reply_token, -1);
                        return;
                    }

                    let reply_msg = Message::new(FS_READ_GRANT, [0, bytes_read, 0, 0, 0, 0], 3);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(_) => send_error_reply(reply_token, -1),
            }
        }

        FS_WRITE => {
            let fid = msg.words[2] as u32;
            let offset = msg.words[3] as u64;
            let len = msg.words[4].min(payload.len());
            let data = &payload[..len];

            match client.write(fid, offset, data) {
                Ok(count) => {
                    let reply_msg = Message::new(FS_WRITE, [0, count as usize, 0, 0, 0, 0], 2);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(_) => send_error_reply(reply_token, -1),
            }
        }

        FS_STAT => {
            let fid = msg.words[1] as u32;
            match client.getattr(fid) {
                Ok(attr) => {
                    let is_dir = (attr.mode & 0o170000) == 0o040000;
                    let flags = if is_dir { 1 } else { 0 } | if !is_dir { 2 } else { 0 };
                    let nlink_uid = ((attr.uid as usize) << 16) | (attr.nlink as usize & 0xFFFF);
                    let reply_msg = Message::new(
                        FS_STAT,
                        [
                            0,
                            attr.size as usize,
                            flags,
                            attr.mtime as usize,
                            nlink_uid,
                            attr.gid as usize,
                        ],
                        6,
                    );
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(_) => send_error_reply(reply_token, -3),
            }
        }

        FS_READDIR => {
            let path = core::str::from_utf8(payload).unwrap_or("");
            match readdir_path(client, path) {
                Ok((data, returned, total)) => {
                    let reply_msg =
                        Message::new(FS_READDIR, [0, 0, returned, total, 0, 0], 5);
                    if let Some(token) = reply_token {
                        if reply_with_payload(token, &reply_msg, &data).is_err() {
                            send_error_reply_shifted(reply_token, -10);
                        }
                    }
                }
                Err(_) => send_error_reply_shifted(reply_token, -3),
            }
        }

        FS_CLOSE => {
            let fid = msg.words[0] as u32;
            let _ = client.clunk(fid);
            let reply_msg = Message::new(FS_CLOSE, [0; 6], 1);
            if let Some(token) = reply_token {
                let _ = reply(token, &reply_msg, IpcFlags::empty());
            }
        }

        FS_UNLINK => {
            let path = core::str::from_utf8(payload).unwrap_or("");
            match unlink_path(client, path, 0) {
                Ok(()) => {
                    let reply_msg = Message::new(FS_UNLINK, [0, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(_) => send_error_reply(reply_token, -1),
            }
        }

        FS_MKDIR => {
            let mode = (msg.words[2] & 0o777) as u32;
            let path = core::str::from_utf8(payload).unwrap_or("");
            match mkdir_path(client, path, mode) {
                Ok(()) => {
                    let reply_msg = Message::new(FS_MKDIR, [0, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(_) => send_error_reply(reply_token, -1),
            }
        }

        FS_RMDIR => {
            let path = core::str::from_utf8(payload).unwrap_or("");
            match unlink_path(client, path, 0x200 /* AT_REMOVEDIR */) {
                Ok(()) => {
                    let reply_msg = Message::new(FS_RMDIR, [0, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(_) => send_error_reply(reply_token, -1),
            }
        }

        FS_CREATE => {
            let mode = (msg.words[2] & 0o777) as u32;
            let path = core::str::from_utf8(payload).unwrap_or("");
            match create_file_path(client, path, mode) {
                Ok(()) => {
                    let reply_msg = Message::new(FS_CREATE, [0, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(_) => send_error_reply(reply_token, -1),
            }
        }

        FS_REALPATH => {
            let path = core::str::from_utf8(payload).unwrap_or("");
            let owned;
            let canon = if path.starts_with('/') {
                path
            } else {
                owned = alloc::format!("/{}", path);
                owned.as_str()
            };
            let bytes = canon.as_bytes();
            let reply_msg = Message::new(FS_REALPATH, [0, bytes.len(), 0, 0, 0, 0], 2);
            if let Some(token) = reply_token {
                let _ = reply_with_payload(token, &reply_msg, bytes);
            }
        }

        _ => {}
    }
}

fn open_path(client: &mut NinepClient, path: &str) -> Result<(u32, u64, bool)> {
    let fid = client.walk_path(path)?;
    match client.lopen(fid, 0x00) {
        Ok(()) => {
            let attr = client.getattr(fid).ok();
            let (is_dir, size) = attr
                .as_ref()
                .map(|a| ((a.mode & 0o170000) == 0o040000, a.size))
                .unwrap_or((false, 0));
            Ok((fid, size, is_dir))
        }
        Err(e) => {
            let _ = client.clunk(fid);
            Err(e)
        }
    }
}

fn readdir_path(client: &mut NinepClient, path: &str) -> Result<(Vec<u8>, usize, usize)> {
    let fid = client.walk_path(path)?;
    if client.lopen(fid, 0x01).is_err() {
        let _ = client.clunk(fid);
        return Err(Error::NotFound);
    }

    let mut all_entries: Vec<(String, bool, u64, u32, u64, u32, u32, u32)> = Vec::new();
    let mut offset = 0u64;
    loop {
        let chunk = match client.readdir(fid, offset, (MSIZE - 64) as u32) {
            Ok(c) => c,
            Err(e) => {
                let _ = client.clunk(fid);
                return Err(e);
            }
        };
        if chunk.is_empty() {
            break;
        }
        let mut pos = 0;
        let mut last_offset = offset;
        while pos + 13 + 8 + 1 + 2 <= chunk.len() {
            pos += 13;
            let entry_offset = u64::from_le_bytes(
                chunk[pos..pos + 8].try_into().unwrap_or([0u8; 8]),
            );
            pos += 8;
            let entry_type = chunk[pos];
            pos += 1;
            let name_len = u16::from_le_bytes(
                chunk[pos..pos + 2].try_into().unwrap_or([0u8; 2]),
            ) as usize;
            pos += 2;
            if pos + name_len > chunk.len() {
                break;
            }
            let name = core::str::from_utf8(&chunk[pos..pos + name_len]).unwrap_or("");
            pos += name_len;
            let is_dir = entry_type == 0;
            let (size, mode, mtime, nlink, uid, gid) =
                match stat_entry(client, path, name) {
                    Ok(a) => (a.size, a.mode, a.mtime, a.nlink as u32, a.uid, a.gid),
                    Err(_) => (
                        0u64,
                        if is_dir { 0o040755 } else { 0o100644 },
                        0u64,
                        1u32,
                        0u32,
                        0u32,
                    ),
                };
            all_entries.push((
                String::from(name),
                is_dir,
                size,
                mode,
                mtime,
                nlink as u32,
                uid,
                gid,
            ));
            last_offset = entry_offset;
        }
        if last_offset <= offset {
            break;
        }
        offset = last_offset;
    }

    let _ = client.clunk(fid);

    const REPLY_BUDGET: usize = 3500;
    let total = all_entries.len();
    let mut data = Vec::new();
    let mut returned = 0usize;
    for (name, is_dir, size, mode, mtime, nlink, uid, gid) in &all_entries {
        let name_bytes = name.as_bytes();
        if name_bytes.len() > 255 {
            continue;
        }
        let mut entry_data = Vec::new();
        entry_data.push(name_bytes.len() as u8);
        entry_data.push(if *is_dir { 1 } else { 0 });
        entry_data.extend_from_slice(&size.to_le_bytes());
        entry_data.extend_from_slice(&mode.to_le_bytes());
        entry_data.extend_from_slice(&mtime.to_le_bytes());
        entry_data.extend_from_slice(&nlink.to_le_bytes());
        entry_data.extend_from_slice(&uid.to_le_bytes());
        entry_data.extend_from_slice(&gid.to_le_bytes());
        entry_data.extend_from_slice(name_bytes);

        if data.len() + entry_data.len() > REPLY_BUDGET {
            break;
        }
        data.extend_from_slice(&entry_data);
        returned += 1;
    }

    Ok((data, returned, total))
}

fn stat_entry(client: &mut NinepClient, dir_path: &str, name: &str) -> Result<GetAttr> {
    let full = if dir_path.is_empty() {
        alloc::format!("{}", name)
    } else {
        alloc::format!("{}/{}", dir_path, name)
    };
    let fid = client.walk_path(&full)?;
    let result = client.getattr(fid);
    let _ = client.clunk(fid);
    result
}

fn unlink_path(client: &mut NinepClient, path: &str, flags: u32) -> Result<()> {
    let (parent_fid, basename) = client.walk_parent(path)?;
    let result = client.unlinkat(parent_fid, &basename, flags);
    let _ = client.clunk(parent_fid);
    result
}

fn mkdir_path(client: &mut NinepClient, path: &str, mode: u32) -> Result<()> {
    let (parent_fid, basename) = client.walk_parent(path)?;
    let result = client.mkdir(parent_fid, &basename, mode | 0o040000, 0);
    let _ = client.clunk(parent_fid);
    result
}

fn create_file_path(client: &mut NinepClient, path: &str, mode: u32) -> Result<()> {
    let (parent_fid, basename) = client.walk_parent(path)?;
    let result = client.lcreate(parent_fid, &basename, 0x02, mode | 0o100000, 0);
    let _ = client.clunk(parent_fid);
    result
}

fn send_error_reply(reply_token: Option<usize>, code: isize) {
    if let Some(token) = reply_token {
        let reply_msg = Message::new(0, [code as usize, 0, 0, 0, 0, 0], 1);
        let _ = reply(token, &reply_msg, IpcFlags::empty());
    }
}

fn send_error_reply_shifted(reply_token: Option<usize>, code: isize) {
    if let Some(token) = reply_token {
        let reply_msg = Message::new(0, [0, code as usize, 0, 0, 0, 0], 2);
        let _ = reply(token, &reply_msg, IpcFlags::empty());
    }
}

fn parse_usize_pair(payload: &[u8]) -> Option<(usize, usize)> {
    if payload.len() < core::mem::size_of::<usize>() * 2 {
        return None;
    }
    let mut bytes = [0u8; core::mem::size_of::<usize>()];
    bytes.copy_from_slice(&payload[..core::mem::size_of::<usize>()]);
    let first = usize::from_ne_bytes(bytes);
    bytes.copy_from_slice(
        &payload[core::mem::size_of::<usize>()..core::mem::size_of::<usize>() * 2],
    );
    let second = usize::from_ne_bytes(bytes);
    Some((first, second))
}
