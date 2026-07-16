#![no_std]
#![no_main]

//! Virtio-blk block device service with filesystem plugin support.
//!
//! This service:
//! 1. Discovers and initializes a virtio-blk PCI device
//! 2. Mounts an ext2 filesystem as a plugin (no IPC overhead)
//! 3. Exposes filesystem operations via IPC for VFS

extern crate alloc;
extern crate cluu_ext2;
extern crate cluu_virtio_blk;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::sync::Arc;
use alloc::vec::Vec;
use cluu_ext2::Ext2Fs;
use cluu_virtio_blk::request_queue::BlkRequestQueue;
use cluu_virtio_blk::session::{pack_cookie, BlkSession};
use cluu_virtio_blk::{DriverState, ModernBlkAdapter, PendingAsync};
use cluu_virtio_core::transport::{FeatureBits, ModernPciTransport, Transport};
use cluu_virtio_core::DmaPool;
use libcluu::boot::{
    process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2, TOKEN_IPC, TOKEN_SPACE,
};
use libcluu::fs::{BlockDevice, Filesystem};
use libcluu::ipc::{
    call, extract_reply_id, reply, reply_with_payload, BLK_CLOSE_SESSION, BLK_OPEN_SESSION,
    BLK_SUBMIT, BLK_SUBMIT_NACK, BLK_TID_CLEANUP, DEVMGR_REGISTER_LABEL,
};
use libcluu::registry;
use libcluu::syscall::{endpoint_create, ipc_recv_any_with_sender, ipc_send, space_map_range};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, space_grant, Error, Result, PAGE_SIZE};

/// IPC labels for filesystem operations (matches VFS protocol)
const FS_OPEN: u32 = 0x300;
const FS_CLOSE: u32 = 0x301;
const FS_READ: u32 = 0x302;
const FS_WRITE: u32 = 0x305;
const FS_STAT: u32 = 0x303;
const FS_READDIR: u32 = 0x304;
const FS_UNLINK: u32 = 0x307;
const FS_MKDIR: u32 = 0x308;
const FS_RMDIR: u32 = 0x309;
const FS_RENAME: u32 = 0x30A;
const FS_CREATE: u32 = 0x30B;
const FS_LINK: u32 = 0x30C;
const FS_REALPATH: u32 = 0x30D;
/// Zero-copy read into a caller-provided mapping (VFS grant buffer).
const FS_READ_GRANT: u32 = 0x306;
const IPC_MESSAGE_MAX: usize = 256;

/// IPC labels for raw block operations (legacy, for debugging)
const BLK_READ_LABEL: u32 = 1;
const BLK_INFO_LABEL: u32 = 3;

/// Fixed grant scratch mapping base for zero-copy reads.
const GRANT_SCRATCH_BASE: usize = 0x6100_0000;
/// Size of the grant scratch buffer (must match VFS GRANT_BUF_SIZE).
const GRANT_SCRATCH_SIZE: usize = 4 * 1024 * 1024;

struct GrantScratch {
    base: usize,
    size: usize,
}

/// Per-process state for the BLK_OPEN_SESSION/SUBMIT/CLOSE protocol. Holds
/// the live sessions and the next monotonic id. Session id `0` is reserved
/// for the legacy FS_READ_GRANT path's cookies (see `session::pack_cookie`).
struct BlkSessionRegistry {
    sessions: BTreeMap<u32, BlkSession>,
    next_session_id: u32,
}

impl BlkSessionRegistry {
    fn new() -> Self {
        Self {
            sessions: BTreeMap::new(),
            next_session_id: 1,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&format!("virtio-blk: error {:?}", e));
            -1
        }
    }
}

fn run() -> Result<()> {
    debug_print("virtio-blk: starting")?;

    let info = process_info();
    let pci_token = info.tokens[TOKEN_EXTRA_1]; // PCI-capable token from init
    let space_token = info.tokens[TOKEN_SPACE];

    debug_print(&format!(
        "virtio-blk: pci_token={} space={}",
        pci_token, space_token
    ))?;

    // === New virtio-core init path ===
    // Address-space carve-out for the new driver. Picked above the legacy
    // mappings so a stale lingering legacy mapping (if any) doesn't clash.
    const DMA_POOL_VA: usize = 0x5100_0000;
    const DMA_POOL_PAGES: usize = 64;
    const MMIO_VA_BASE: usize = 0x5200_0000;
    const READ_SCRATCH_BASE: usize = 0x6200_0000;
    const READ_SCRATCH_PAGES: usize = 1024; // 4 MiB

    // virtio-blk = vendor 0x1AF4, devices 0x1001 (transitional) + 0x1042 (modern).
    // We only accept the modern variant for the new stack.
    let pci_device = match cluu_virtio_core::pci::find_virtio_device(
        pci_token,
        &[0x1001, 0x1042],
        &[0x1042],
    ) {
        Ok(d) => {
            debug_print(&format!("virtio-blk: found PCI device {:?}", d))?;
            d
        }
        Err(e) => {
            debug_print(&format!("virtio-blk: find_virtio_device failed: {:?}", e))?;
            return Err(e);
        }
    };

    cluu_virtio_core::pci::enable_device(pci_token, &pci_device)?;
    // Read back PCI command register to verify bus master + memory space enabled.
    let cmd_status = libcluu::syscall::pci_config_read(
        pci_token, pci_device.bus, pci_device.device, pci_device.function, 0x04,
    )?;
    debug_print(&format!(
        "virtio-blk: PCI command={:#06x} status={:#06x}",
        cmd_status & 0xFFFF,
        (cmd_status >> 16) & 0xFFFF
    ))?;

    let pool = DmaPool::new(space_token, DMA_POOL_VA, DMA_POOL_PAGES)?;

    // The four virtio cap regions live in `cap_bar` (typically BAR4 on QEMU's
    // transitional virtio-blk). `cap_bar_phys` / `cap_bar_size` were resolved
    // by `find_virtio_device` after the cap walk; `bar0` here is a legacy
    // I/O port BAR we don't use on the modern path.
    let bar_phys = pci_device.cap_bar_phys;
    let bar_size = pci_device.cap_bar_size;
    let mut transport = ModernPciTransport::new(
        space_token,
        pci_device.clone(),
        bar_phys,
        bar_size,
        MMIO_VA_BASE,
    )?;

    // Reset, negotiate features (VERSION_1 only — no fancy device features yet).
    transport.reset()?;
    let dev_feats = transport.read_device_features()?;
    let want = FeatureBits::VERSION_1.bits() & dev_feats;
    transport.write_driver_features(want)?;

    // Read device capacity from device_cfg (capacity at offset 0, u64).
    // virtio 1.2 §5.2.4 — also has blk_size at offset 20, but we use the
    // standard 512 sector size.
    let device_cfg_va = transport.device_cfg_va;
    let capacity_sectors = unsafe { core::ptr::read_volatile(device_cfg_va as *const u64) };
    debug_print(&format!(
        "virtio-blk: capacity_sectors={}",
        capacity_sectors
    ))?;

    let mut bq = BlkRequestQueue::new(transport, pool, 256)?;
    bq.transport.set_driver_ok()?;

    // Pre-map the scratch buffer for read DMA. Single-in-flight at this
    // stage (no IPC concurrency yet) means no contention on the buffer.
    space_map_range(
        space_token,
        READ_SCRATCH_BASE,
        0,
        0x03, // R+W
        READ_SCRATCH_PAGES,
        0,
    )?;

    // Read the PCI Interrupt Line register (offset 0x3C low byte) to
    // discover which legacy IRQ the device uses on this QEMU topology.
    let intr_line_word = libcluu::syscall::pci_config_read(
        pci_token,
        pci_device.bus,
        pci_device.device,
        pci_device.function,
        0x3c,
    )?;
    let irq_number = (intr_line_word & 0xFF) as usize;
    debug_print(&format!(
        "virtio-blk: PCI Interrupt Line = {} (raw 0x{:08x})",
        irq_number, intr_line_word
    ))?;

    let irq_token = info.tokens[TOKEN_EXTRA_2];
    let ipc_token = info.tokens[TOKEN_IPC];
    let irq = cluu_virtio_core::IrqSource::new(ipc_token, irq_token, irq_number)?;
    let _ = debug_print(&format!(
        "virtio-blk: IRQ attached (endpoint={} irq={})",
        irq.endpoint, irq.irq_number
    ));

    let state = Arc::new(DriverState::new(bq));

    let adapter = ModernBlkAdapter::new(
        state.clone(),
        capacity_sectors,
        512,
        READ_SCRATCH_BASE,
        READ_SCRATCH_PAGES,
        space_token,
    );

    debug_print(&format!(
        "virtio-blk: virtio-core stack initialized ({} sectors, {} bytes)",
        capacity_sectors,
        capacity_sectors * 512
    ))?;

    // Initialize registry and create listen endpoint BEFORE the recv worker
    // (it needs all three endpoints in WORKER_CTX) and BEFORE ext2 mount
    // (which calls into adapter.read_bytes — that path needs the worker to
    // drain IRQs and signal sync completions).
    registry::init("blkdev")?;

    let listen_endpoint = info.tokens[TOKEN_EXTRA_0];
    let listen_endpoint = if listen_endpoint != 0 {
        listen_endpoint
    } else {
        endpoint_create(pci_token)?
    };

    registry::register_output("main", listen_endpoint)?;
    debug_print("virtio-blk: registered as blkdev:main")?;

    let registry_endpoint = registry::control_endpoint();
    let grant_scratch = map_grant_scratch(space_token)?;

    let fs = match Ext2Fs::mount(&adapter) {
        Ok(fs) => {
            debug_print("virtio-blk: ext2 filesystem mounted")?;
            Some(fs)
        }
        Err(e) => {
            debug_print(&format!(
                "virtio-blk: no ext2 found ({:?}), raw block only",
                e
            ))?;
            None
        }
    };

    register_with_devmgr(capacity_sectors);

    // Main service loop: listen + irq + registry on a single thread.
    let mut sessions = BlkSessionRegistry::new();
    let mut buf = [0u8; 4096];
    loop {
        let tokens = [listen_endpoint, irq.endpoint, registry_endpoint];
        let (idx, len, sender_tid) = match ipc_recv_any_with_sender(&tokens, &mut buf, 50) {
            Ok(t) => t,
            Err(libcluu::Error::Timeout) | Err(libcluu::Error::WouldBlock) => {
                state.drain_and_route();
                continue;
            }
            Err(_) => continue,
        };

        if idx == 1 {
            // IRQ wake: drain device, dispatch async BLK_COMPLETEs.
            // sync_completions is unused in single-thread mode; read_bytes
            // drains the queue itself directly during its wait loop.
            state.drain_and_route();
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

        if dispatch_blk_session(&state, &mut sessions, msg, payload, sender_tid) {
            continue;
        }

        if let Some(ref fs) = fs {
            handle_fs_request(fs, &adapter, space_token, &grant_scratch, msg, payload);
        } else {
            handle_block_request(&adapter, msg);
        }
    }
}

fn register_with_devmgr(capacity_sectors: u64) {
    let devmgr_ep = match registry::subscribe_output("devmgr", "main") {
        Ok(ep) => ep,
        Err(e) => {
            let _ = debug_print(&format!(
                "virtio-blk: devmgr subscribe failed {:?} — continuing without registration",
                e
            ));
            return;
        }
    };
    let mut msg = Message::new(
        DEVMGR_REGISTER_LABEL,
        [0, capacity_sectors as usize, 0, 0, 0, 0],
        2,
    );
    match call(devmgr_ep, &mut msg, IpcFlags::empty()) {
        Ok(()) => {
            let _ = debug_print(&format!(
                "virtio-blk: registered with devmgr ({} sectors, status={})",
                capacity_sectors, msg.words[0]
            ));
        }
        Err(e) => {
            let _ = debug_print(&format!(
                "virtio-blk: devmgr register call failed {:?}",
                e
            ));
        }
    }
}

fn handle_fs_request(
    fs: &Ext2Fs,
    blk: &ModernBlkAdapter,
    space_token: usize,
    grant_scratch: &GrantScratch,
    msg: &Message,
    payload: &[u8],
) {
    let reply_token = extract_reply_id(msg);

    match msg.tag.label {
        FS_OPEN => {
            // payload = path, words[1] = client_id (unused here)
            let path = core::str::from_utf8(payload).unwrap_or("");
            match fs.resolve_path(path) {
                Ok(inode) => {
                    match fs.stat(inode) {
                        Ok(stat) => {
                            let reply_msg = Message::new(
                                FS_OPEN,
                                [0, inode as usize, stat.size as usize, 0, 0, 0],
                                3,
                            );
                            if let Some(token) = reply_token {
                                let _ = reply(token, &reply_msg, IpcFlags::empty());
                            }
                        }
                        Err(_) => send_error_reply(reply_token, -3), // NotFound
                    }
                }
                Err(_) => send_error_reply(reply_token, -3), // NotFound
            }
        }

        FS_READ => {
            // words[1] = client_id, words[2] = inode, words[3] = offset, words[4] = len
            let inode = msg.words[2] as u64;
            let offset = msg.words[3] as u64;
            let len = msg.words[4].min(IPC_MESSAGE_MAX - core::mem::size_of::<Message>());

            let mut read_buf = alloc::vec![0u8; len];
            match fs.read(inode, offset, &mut read_buf) {
                Ok(bytes_read) => {
                    let reply_msg = Message::new(FS_READ, [0, 0, bytes_read, 0, 0, 0], 3);
                    if let Some(token) = reply_token {
                        let _ = reply_with_payload(token, &reply_msg, &read_buf[..bytes_read]);
                    }
                }
                Err(_) => send_error_reply_shifted(reply_token, -1),
            }
        }

        FS_WRITE => {
            // words[2] = inode, words[3] = offset, words[4] = len; payload = bytes
            let inode = msg.words[2] as u64;
            let offset = msg.words[3] as u64;
            let len = msg.words[4].min(payload.len());
            let data = &payload[..len];

            match fs.write_by_inode(inode, offset, data) {
                Ok(bytes_written) => {
                    let reply_msg = Message::new(FS_WRITE, [0, bytes_written, 0, 0, 0, 0], 2);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(_) => send_error_reply(reply_token, -1),
            }
        }

        FS_READ_GRANT => {
            // words[2] = inode, words[3] = offset, words[4] = len
            let Some((target_base, target_space)) = parse_usize_pair(payload) else {
                send_error_reply(reply_token, -2);
                return;
            };

            let inode = msg.words[2] as u64;
            let offset = msg.words[3] as u64;
            let len = msg.words[4];
            if len == 0 {
                let reply_msg = Message::new(FS_READ_GRANT, [0, 0, 0, 0, 0, 0], 2);
                if let Some(token) = reply_token {
                    let _ = reply(token, &reply_msg, IpcFlags::empty());
                }
                return;
            }

            if len > grant_scratch.size {
                send_error_reply(reply_token, -4);
                return;
            }

            let scratch = unsafe {
                core::slice::from_raw_parts_mut(grant_scratch.base as *mut u8, grant_scratch.size)
            };
            match fs.read(inode, offset, &mut scratch[..len]) {
                Ok(bytes_read) => {
                    if bytes_read == 0 {
                        let reply_msg = Message::new(FS_READ_GRANT, [0, 0, 0, 0, 0, 0], 2);
                        if let Some(token) = reply_token {
                            let _ = reply(token, &reply_msg, IpcFlags::empty());
                        }
                        return;
                    }
                    let pages = bytes_read.div_ceil(PAGE_SIZE);

                    let mut grant_err = None;
                    for page_idx in 0..pages {
                        let src = grant_scratch.base + page_idx * PAGE_SIZE;
                        let dst = target_base + page_idx * PAGE_SIZE;
                        // Grant writable pages so VFS can reuse the buffer safely.
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

        FS_STAT => {
            // words[1] = inode
            let inode = msg.words[1] as u64;
            match fs.stat(inode) {
                Ok(stat) => {
                    let flags = if stat.is_dir { 1 } else { 0 } | if stat.is_file { 2 } else { 0 };
                    // Pack nlink (low 16) and uid (high 16) into one word.
                    let nlink_uid = ((stat.uid as usize) << 16) | (stat.nlink as usize & 0xFFFF);
                    let reply_msg = Message::new(
                        FS_STAT,
                        [
                            0,
                            stat.size as usize,
                            flags,
                            stat.mtime as usize,
                            nlink_uid,
                            stat.gid as usize,
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

        FS_REALPATH => {
            let path_raw = core::str::from_utf8(payload).unwrap_or("");
            // VFS RemoteBackend strips the mount prefix and leading slash before
            // forwarding (e.g. "/bin/ls" → "bin/ls"); realpath_canonical wants an
            // absolute path. Re-prepend "/" when needed.
            let owned;
            let path = if path_raw.starts_with('/') {
                path_raw
            } else {
                owned = alloc::format!("/{}", path_raw);
                owned.as_str()
            };
            match fs.realpath_canonical(path) {
                Ok((canon, _inode)) => {
                    let bytes = canon.into_bytes();
                    let max_bytes = IPC_MESSAGE_MAX.saturating_sub(core::mem::size_of::<Message>());
                    if bytes.len() > max_bytes {
                        // path too long; no NameTooLong variant in libcluu::Error,
                        // fall back to InvalidArgument (-1).
                        send_error_reply(reply_token, Error::InvalidArgument.to_errno());
                    } else {
                        let reply_msg = Message::new(FS_REALPATH, [0, bytes.len(), 0, 0, 0, 0], 2);
                        if let Some(token) = reply_token {
                            let _ = reply_with_payload(token, &reply_msg, &bytes);
                        }
                    }
                }
                Err(err) => send_error_reply(reply_token, err.to_errno()),
            }
        }

        FS_READDIR => {
            let path = core::str::from_utf8(payload).unwrap_or("");
            let start_offset = msg.words[2] as usize;
            const REPLY_BUDGET: usize = 3500;
            match fs.resolve_path(path) {
                Ok(inode) => {
                    match fs.readdir(inode) {
                        Ok(entries) => {
                            let total = entries.len();
                            let mut data = Vec::new();
                            let mut returned = 0usize;
                            for entry in entries.iter().skip(start_offset) {
                                let name_bytes = entry.name.as_bytes();
                                if name_bytes.len() > 255 {
                                    continue;
                                }
                                let stat = fs.stat(entry.inode).ok();
                                let (size, mode, mtime, nlink, uid, gid) = match stat {
                                    Some(s) => {
                                        let mode = if s.is_dir { 0o040755u32 } else { 0o100644u32 };
                                        (s.size, mode, s.mtime, s.nlink, s.uid, s.gid)
                                    }
                                    None => {
                                        let mode = if entry.is_dir { 0o040755u32 } else { 0o100644u32 };
                                        (0u64, mode, 0u64, 1u32, 0u32, 0u32)
                                    }
                                };
                                let mut entry_data = Vec::new();
                                entry_data.push(name_bytes.len() as u8);
                                entry_data.push(if entry.is_dir { 1 } else { 0 });
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

                            let reply_msg =
                                Message::new(FS_READDIR, [0, 0, returned, total, 0, 0], 5);
                            if let Some(token) = reply_token {
                                if reply_with_payload(token, &reply_msg, &data).is_err() {
                                    let _ = libcluu::debug_print(&format!(
                                        "blkdev: FS_READDIR reply failed ({} entries, {} bytes — may exceed IPC_MESSAGE_MAX)",
                                        returned,
                                        data.len()
                                    ));
                                    send_error_reply_shifted(reply_token, -10);
                                }
                            }
                        }
                        Err(_) => send_error_reply_shifted(reply_token, -1),
                    }
                }
                Err(_) => send_error_reply_shifted(reply_token, -3),
            }
        }

        FS_UNLINK => {
            let path = core::str::from_utf8(payload).unwrap_or("");
            match fs.unlink_path(path) {
                Ok(()) => {
                    let reply_msg = Message::new(FS_UNLINK, [0, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(err) => send_error_reply(reply_token, err.to_errno()),
            }
        }

        FS_MKDIR => {
            let mode = (msg.words[2] & 0o777) as u16;
            let path = core::str::from_utf8(payload).unwrap_or("");
            match fs.mkdir_path(path, mode) {
                Ok(()) => {
                    let reply_msg = Message::new(FS_MKDIR, [0, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(err) => send_error_reply(reply_token, err.to_errno()),
            }
        }

        FS_RMDIR => {
            let path = core::str::from_utf8(payload).unwrap_or("");
            match fs.rmdir_path(path) {
                Ok(()) => {
                    let reply_msg = Message::new(FS_RMDIR, [0, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(err) => send_error_reply(reply_token, err.to_errno()),
            }
        }

        FS_RENAME => {
            let old_len = msg.words[2];
            if old_len > payload.len() {
                send_error_reply(reply_token, -2);
                return;
            }
            let old = core::str::from_utf8(&payload[..old_len]).unwrap_or("");
            let new = core::str::from_utf8(&payload[old_len..]).unwrap_or("");
            match fs.rename_path(old, new) {
                Ok(()) => {
                    let reply_msg = Message::new(FS_RENAME, [0, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(err) => send_error_reply(reply_token, err.to_errno()),
            }
        }

        FS_CREATE => {
            let mode = (msg.words[2] & 0o777) as u16;
            let path = core::str::from_utf8(payload).unwrap_or("");
            match fs.create_file_path(path, mode) {
                Ok(_) => {
                    let reply_msg = Message::new(FS_CREATE, [0, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(err) => send_error_reply(reply_token, err.to_errno()),
            }
        }

        FS_LINK => {
            let old_len = msg.words[2];
            if old_len > payload.len() {
                send_error_reply(reply_token, -2);
                return;
            }
            let old = core::str::from_utf8(&payload[..old_len]).unwrap_or("");
            let new = core::str::from_utf8(&payload[old_len..]).unwrap_or("");
            match fs.link_path(old, new) {
                Ok(()) => {
                    let reply_msg = Message::new(FS_LINK, [0, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(err) => send_error_reply(reply_token, err.to_errno()),
            }
        }

        FS_CLOSE => {
            // No-op for now (stateless)
            let reply_msg = Message::new(FS_CLOSE, [0; 6], 1);
            if let Some(token) = reply_token {
                let _ = reply(token, &reply_msg, IpcFlags::empty());
            }
        }

        // Legacy block operations for backwards compatibility
        BLK_INFO_LABEL => {
            let sector_count = blk.sector_count();
            let sector_size = blk.sector_size();
            let reply_msg = Message::new(
                BLK_INFO_LABEL,
                [
                    (sector_count & 0xFFFFFFFF) as usize,
                    (sector_count >> 32) as usize,
                    sector_size,
                    0,
                    0,
                    0,
                ],
                3,
            );
            if let Some(token) = reply_token {
                let _ = reply(token, &reply_msg, IpcFlags::empty());
            }
        }

        BLK_READ_LABEL => {
            let start = msg.words[0] as u64;
            let count = msg.words[1];
            let byte_count = count * 512;

            if byte_count > 4096 - 64 {
                send_error_reply_shifted(reply_token, -1);
                return;
            }

            let mut data_buf = alloc::vec![0u8; byte_count];
            match blk.read_bytes(start * 512, &mut data_buf) {
                Ok(bytes_read) => {
                    let reply_msg = Message::new(BLK_READ_LABEL, [0, bytes_read, 0, 0, 0, 0], 2);
                    if let Some(token) = reply_token {
                        let _ = reply_with_payload(token, &reply_msg, &data_buf);
                    }
                }
                Err(_) => send_error_reply_shifted(reply_token, -1),
            }
        }

        _ => {}
    }
}

fn map_grant_scratch(space_token: usize) -> Result<GrantScratch> {
    let pages = GRANT_SCRATCH_SIZE.div_ceil(PAGE_SIZE);
    match space_map_range(space_token, GRANT_SCRATCH_BASE, 0, 0x03, pages, 0) {
        Ok(_) | Err(libcluu::Error::AlreadyExists) => Ok(GrantScratch {
            base: GRANT_SCRATCH_BASE,
            size: GRANT_SCRATCH_SIZE,
        }),
        Err(err) => Err(err),
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

fn handle_block_request(blk: &ModernBlkAdapter, msg: &Message) {
    let reply_token = extract_reply_id(msg);

    match msg.tag.label {
        BLK_INFO_LABEL => {
            let sector_count = blk.sector_count();
            let sector_size = blk.sector_size();
            let reply_msg = Message::new(
                BLK_INFO_LABEL,
                [
                    (sector_count & 0xFFFFFFFF) as usize,
                    (sector_count >> 32) as usize,
                    sector_size,
                    0,
                    0,
                    0,
                ],
                3,
            );
            if let Some(token) = reply_token {
                let _ = reply(token, &reply_msg, IpcFlags::empty());
            }
        }

        BLK_READ_LABEL => {
            let start = msg.words[0] as u64;
            let count = msg.words[1];
            let byte_count = count * 512;

            if byte_count > 4096 - 64 {
                send_error_reply_shifted(reply_token, -1);
                return;
            }

            let mut data_buf = alloc::vec![0u8; byte_count];
            match blk.read_bytes(start * 512, &mut data_buf) {
                Ok(bytes_read) => {
                    let reply_msg = Message::new(BLK_READ_LABEL, [0, bytes_read, 0, 0, 0, 0], 2);
                    if let Some(token) = reply_token {
                        let _ = reply_with_payload(token, &reply_msg, &data_buf);
                    }
                }
                Err(_) => send_error_reply_shifted(reply_token, -1),
            }
        }

        _ => {}
    }
}

fn send_error_reply(reply_token: Option<usize>, code: isize) {
    if let Some(token) = reply_token {
        let reply_msg = Message::new(0, [code as usize, 0, 0, 0, 0, 0], 1);
        let _ = reply(token, &reply_msg, IpcFlags::empty());
    }
}

/// Error reply with errno in words[1] (for labels that shifted status to words[1+]).
fn send_error_reply_shifted(reply_token: Option<usize>, code: isize) {
    if let Some(token) = reply_token {
        let reply_msg = Message::new(0, [0, code as usize, 0, 0, 0, 0], 2);
        let _ = reply(token, &reply_msg, IpcFlags::empty());
    }
}

/// Recv-worker entry point for the dedicated BLK/IRQ/registry dispatch
/// thread. Spawned by `run()` after `WORKER_CTX` is initialized; never
/// returns. `extern "C"` because `thread_create` jumps to a raw address
/// using the SysV calling convention.
/// Top-level demux for the BLK_OPEN_SESSION / BLK_SUBMIT / BLK_CLOSE_SESSION
/// protocol. Returns `true` when the message has been handled (or
/// definitively malformed and dropped); `false` means the caller should
/// forward to the main thread (FS_*/legacy block dispatch).
///
/// BLK_SUBMIT is fire-and-forget here: register the cookie under
/// `state.pending`, submit + notify, return without replying. The IRQ
/// path (`drain_and_route`) delivers the eventual `BLK_COMPLETE`.
fn dispatch_blk_session(
    state: &DriverState,
    sessions: &mut BlkSessionRegistry,
    msg: &Message,
    payload: &[u8],
    sender_tid: usize,
) -> bool {
    let reply_token = extract_reply_id(msg);
    match msg.tag.label {
        BLK_OPEN_SESSION => {
            let comp_ep = msg.words[0];
            let sid = sessions.next_session_id;
            sessions.next_session_id = sessions.next_session_id.wrapping_add(1);
            if sessions.next_session_id == 0 {
                sessions.next_session_id = 1;
            }
            sessions
                .sessions
                .insert(sid, BlkSession::new(sid, comp_ep, sender_tid));

            let reply_msg = Message::new(BLK_OPEN_SESSION, [0, sid as usize, 0, 0, 0, 0], 2);
            if let Some(rt) = reply_token {
                let _ = reply(rt, &reply_msg, IpcFlags::empty());
            }
            true
        }

        BLK_CLOSE_SESSION => {
            let sid = msg.words[0] as u32;
            sessions.sessions.remove(&sid);
            true
        }

        BLK_TID_CLEANUP => {
            // Procmgr broadcast: the named tid has exited. Reap any sessions
            // owned by it. Authoritative — we trust procmgr's tid claim;
            // sender_tid auth on the channel limits who can issue this.
            let dead_tid = msg.words[0];
            let to_drop: alloc::vec::Vec<u32> = sessions
                .sessions
                .iter()
                .filter_map(|(sid, s)| if s.owner_tid == dead_tid { Some(*sid) } else { None })
                .collect();
            for sid in to_drop {
                sessions.sessions.remove(&sid);
                let _ = libcluu::debug_print(&format!(
                    "virtio-blk: session {} reaped (tid={})", sid, dead_tid
                ));
            }
            true
        }

        BLK_SUBMIT => {
            let sid = msg.words[0] as u32;
            let rid = msg.words[1] as u64;
            let lba = ((msg.words[3] as u64) << 32) | (msg.words[2] as u64);
            let n_pages = msg.words[4];
            let total_bytes = msg.words[5];

            let comp_ep = match sessions.sessions.get(&sid) {
                Some(s) => s.completion_endpoint,
                None => return true, // unknown session — drop
            };

            if payload.len() < 8 * n_pages {
                send_blk_nack(comp_ep, rid, Error::InvalidArgument);
                return true;
            }

            let mut pages: Vec<u64> = Vec::with_capacity(n_pages);
            for i in 0..n_pages {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&payload[i * 8..i * 8 + 8]);
                pages.push(u64::from_le_bytes(bytes));
            }

            let cookie = pack_cookie(sid, rid);
            let submit_err = {
                let mut inner = state.inner.lock();
                inner.pending.insert(cookie, PendingAsync { comp_ep, rid });
                match inner.bq.submit_read(lba, &pages, total_bytes, cookie) {
                    Ok(()) => {
                        inner.bq.notify();
                        None
                    }
                    Err(e) => {
                        inner.pending.remove(&cookie);
                        Some(e)
                    }
                }
            };
            if submit_err.is_some() {
                send_blk_nack(comp_ep, rid, Error::Busy);
            }
            true
        }

        _ => false,
    }
}

/// Send a BLK_SUBMIT_NACK message — used when the request was rejected
/// before it ever hit the device (e.g. malformed payload).
fn send_blk_nack(comp_ep: usize, rid: u64, err: Error) {
    let msg = Message::new(
        BLK_SUBMIT_NACK,
        [rid as usize, err as isize as usize, 0, 0, 0, 0],
        2,
    );
    let _ = ipc_send(comp_ep, msg.as_bytes());
}
