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
use alloc::vec::Vec;
use cluu_ext2::Ext2Fs;
use cluu_virtio_blk::request_queue::BlkRequestQueue;
use cluu_virtio_blk::session::{pack_cookie, BlkSession};
use cluu_virtio_blk::ModernBlkAdapter;
use cluu_virtio_core::transport::{FeatureBits, ModernPciTransport, Transport};
use cluu_virtio_core::DmaPool;
use libcluu::boot::{
    process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2, TOKEN_IPC, TOKEN_SPACE,
};
use libcluu::fs::{BlockDevice, Filesystem};
use libcluu::ipc::{
    extract_reply_id, reply, reply_with_payload, BLK_CLOSE_SESSION, BLK_COMPLETE, BLK_OPEN_SESSION,
    BLK_SUBMIT, BLK_SUBMIT_NACK,
};
use libcluu::registry;
use libcluu::syscall::{endpoint_create, ipc_recv_any, ipc_send, space_map_range};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, space_grant, yield_cpu, Error, Result, PAGE_SIZE};

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

/// Outcome of a synchronous BLK_SUBMIT dispatch.
enum BlkSubmitOutcome {
    Ok { bytes_done: usize },
    Failed,
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

    // Match the device's max queue size (256 on QEMU). Smaller queues
    // surface a wrap-around bug we haven't fully diagnosed; using the
    // device max sidesteps it for now and gives us more in-flight depth.
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

    let adapter = ModernBlkAdapter::new(
        bq,
        capacity_sectors,
        512,
        READ_SCRATCH_BASE,
        READ_SCRATCH_PAGES,
        irq.endpoint,
    );

    debug_print(&format!(
        "virtio-blk: virtio-core stack initialized ({} sectors, {} bytes)",
        capacity_sectors,
        capacity_sectors * 512
    ))?;

    // Try to mount ext2 filesystem
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

    // Initialize registry and create listen endpoint
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

    let mut blk_sessions = BlkSessionRegistry::new();

    // Main service loop
    let mut buf = [0u8; 4096];
    loop {
        let tokens = [listen_endpoint, registry_endpoint];
        match ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                if len < core::mem::size_of::<Message>() {
                    continue;
                }

                let msg = unsafe { &*(buf.as_ptr() as *const Message) };
                let payload = &buf[core::mem::size_of::<Message>()..len];

                if index == 1 {
                    // Registry message
                    let _ = registry::handle_incoming_message(msg, payload);
                    continue;
                }

                // Intercept new BLK_* session protocol before falling through
                // to the FS / legacy block dispatchers.
                if handle_blk_session_message(&adapter, &mut blk_sessions, msg, payload) {
                    continue;
                }

                // Handle request
                if let Some(ref fs) = fs {
                    handle_fs_request(fs, &adapter, space_token, &grant_scratch, msg, payload);
                } else {
                    handle_block_request(&adapter, msg);
                }
            }
            Err(_) => {
                let _ = yield_cpu();
            }
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
                    let reply_msg =
                        Message::new(FS_STAT, [0, stat.size as usize, flags, 0, 0, 0], 3);
                    if let Some(token) = reply_token {
                        let _ = reply(token, &reply_msg, IpcFlags::empty());
                    }
                }
                Err(_) => send_error_reply(reply_token, -3),
            }
        }

        FS_READDIR => {
            // payload = path
            let path = core::str::from_utf8(payload).unwrap_or("");
            match fs.resolve_path(path) {
                Ok(inode) => {
                    match fs.readdir(inode) {
                        Ok(entries) => {
                            // Serialize entries
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

                            let reply_msg =
                                Message::new(FS_READDIR, [0, 0, entries.len(), 0, 0, 0], 3);
                            if let Some(token) = reply_token {
                                let _ = reply_with_payload(token, &reply_msg, &data);
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

/// Top-level demux for the new BLK_OPEN_SESSION / BLK_SUBMIT /
/// BLK_CLOSE_SESSION protocol. Returns `true` when the message has been
/// handled (or definitively malformed and dropped); `false` means the
/// caller should route to the FS / legacy block dispatchers.
///
/// Dispatch is synchronous: a BLK_SUBMIT locks the BlkRequestQueue mutex,
/// submits, blocks on the IRQ endpoint, drains, and sends BLK_COMPLETE
/// before returning. This serializes all BLK_* requests with each other
/// AND with the FS_READ_GRANT path (which holds the same mutex through
/// `ModernBlkAdapter::read_bytes`). T5.7 is needed for real concurrency.
fn handle_blk_session_message(
    adapter: &ModernBlkAdapter,
    sessions: &mut BlkSessionRegistry,
    msg: &Message,
    payload: &[u8],
) -> bool {
    let reply_token = extract_reply_id(msg);
    match msg.tag.label {
        BLK_OPEN_SESSION => {
            // words[0] = caller's completion endpoint token
            let comp_ep = msg.words[0];
            let sid = sessions.next_session_id;
            sessions.next_session_id = sessions.next_session_id.wrapping_add(1);
            // Skip 0 if the wrap ever circles back; 0 is reserved for FS path.
            if sessions.next_session_id == 0 {
                sessions.next_session_id = 1;
            }
            sessions
                .sessions
                .insert(sid, BlkSession::new(sid, comp_ep));

            // Reply: words[0] = errno (0 = OK), words[1] = session_id.
            let reply_msg = Message::new(
                BLK_OPEN_SESSION,
                [0, sid as usize, 0, 0, 0, 0],
                2,
            );
            if let Some(rt) = reply_token {
                let _ = reply(rt, &reply_msg, IpcFlags::empty());
            }
            true
        }

        BLK_CLOSE_SESSION => {
            let sid = msg.words[0] as u32;
            sessions.sessions.remove(&sid);
            // CLOSE is fire-and-forget per design; no reply.
            true
        }

        BLK_SUBMIT => {
            // words[0] = session_id, words[1] = request_id (low 32 bits used)
            // words[2..3] = lba (low,high), words[4] = n_pages, words[5] = total_bytes
            // payload = n_pages * u64 LE physical-page addresses.
            let sid = msg.words[0] as u32;
            let rid = msg.words[1] as u64;
            let lba = ((msg.words[3] as u64) << 32) | (msg.words[2] as u64);
            let n_pages = msg.words[4];
            let total_bytes = msg.words[5];

            // Look up the session — silently drop unknown sids (caller leaked).
            let comp_ep = match sessions.sessions.get(&sid) {
                Some(s) => s.completion_endpoint,
                None => return true,
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
            let outcome = run_blk_submit(adapter, lba, &pages, total_bytes, cookie);

            match outcome {
                BlkSubmitOutcome::Ok { bytes_done } => {
                    send_blk_complete(comp_ep, rid, 0, bytes_done);
                }
                BlkSubmitOutcome::Failed => {
                    send_blk_complete(comp_ep, rid, 1, 0);
                }
            }
            true
        }

        _ => false,
    }
}

/// Synchronously submit a read on the BlkRequestQueue, wait for the IRQ,
/// and drain completions until ours pops out. Holds the BlkRequestQueue
/// mutex (and incidentally the IRQ endpoint by virtue of the single
/// dispatch thread) for the entire duration.
fn run_blk_submit(
    adapter: &ModernBlkAdapter,
    lba: u64,
    pages: &[u64],
    total_bytes: usize,
    cookie: u64,
) -> BlkSubmitOutcome {
    let mut bq = adapter.inner.lock();
    if bq.submit_read(lba, pages, total_bytes, cookie).is_err() {
        return BlkSubmitOutcome::Failed;
    }
    bq.notify();

    let tokens = [adapter.irq_endpoint];
    let mut irq_buf = [0u8; 64];
    loop {
        if ipc_recv_any(&tokens, &mut irq_buf, u64::MAX).is_err() {
            return BlkSubmitOutcome::Failed;
        }
        let _ = bq.transport.isr_status();
        for (got, status, blen) in bq.drain_completions() {
            if got == cookie {
                if status == 0 {
                    return BlkSubmitOutcome::Ok {
                        bytes_done: blen as usize,
                    };
                } else {
                    return BlkSubmitOutcome::Failed;
                }
            }
            // Other cookies cannot happen in this synchronous design (the
            // BlkRequestQueue mutex is held end-to-end), but if a stale
            // completion ever leaked through we have nowhere to deliver
            // it — drop on the floor and keep waiting for ours.
        }
    }
}

/// Send a BLK_COMPLETE message to the caller's completion endpoint.
/// `status`: 0 = success, non-zero = device/driver failure.
fn send_blk_complete(comp_ep: usize, rid: u64, status: u8, bytes_done: usize) {
    let msg = Message::new(
        BLK_COMPLETE,
        [rid as usize, status as usize, bytes_done, 0, 0, 0],
        3,
    );
    let _ = ipc_send(comp_ep, msg.as_bytes());
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
