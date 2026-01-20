#![no_std]
#![no_main]

//! Virtio-blk block device service with filesystem plugin support.
//!
//! This service:
//! 1. Discovers and initializes a virtio-blk PCI device
//! 2. Mounts an ext2 filesystem as a plugin (no IPC overhead)
//! 3. Exposes filesystem operations via IPC for VFS

extern crate alloc;
extern crate cluu_virtio_blk;
extern crate cluu_ext2;

use alloc::format;
use alloc::vec::Vec;
use cluu_ext2::Ext2Fs;
use cluu_virtio_blk::{pci, VirtioBlkAdapter, VirtioBlkDevice};
use libcluu::boot::{process_info, TOKEN_SPACE};

/// Token slot where init places the PCI-capable token
const SVC_TOKEN_CAP: usize = 8;
use libcluu::fs::{BlockDevice, Filesystem};
use libcluu::ipc::{extract_reply_token, reply, reply_with_payload};
use libcluu::registry;
use libcluu::syscall::{endpoint_create, ipc_recv_any};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Result};

/// IPC labels for filesystem operations (matches VFS protocol)
const FS_OPEN: u32 = 0x300;
const FS_CLOSE: u32 = 0x301;
const FS_READ: u32 = 0x302;
const FS_STAT: u32 = 0x303;
const FS_READDIR: u32 = 0x304;

/// IPC labels for raw block operations (legacy, for debugging)
const BLK_READ_LABEL: u32 = 1;
const BLK_INFO_LABEL: u32 = 3;

/// Token slot for the service's listen endpoint
const SVC_TOKEN_LISTEN: usize = 7;

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
    let pci_token = info.tokens[SVC_TOKEN_CAP]; // PCI-capable token from init
    let space_token = info.tokens[TOKEN_SPACE];

    debug_print(&format!("virtio-blk: pci_token={} space={}", pci_token, space_token))?;

    // Find virtio-blk PCI device
    let pci_addr = match pci::find_virtio_blk(pci_token) {
        Ok(addr) => {
            debug_print(&format!("virtio-blk: found PCI device at {:?}", addr))?;
            addr
        }
        Err(e) => {
            debug_print(&format!("virtio-blk: find_virtio_blk failed: {:?}", e))?;
            return Err(e);
        }
    };

    // Initialize the virtio-blk device
    debug_print("virtio-blk: creating device...")?;
    let device = match VirtioBlkDevice::new(pci_token, space_token, pci_addr) {
        Ok(dev) => {
            debug_print("virtio-blk: device created")?;
            dev
        }
        Err(e) => {
            debug_print(&format!("virtio-blk: failed to init device: {:?}", e))?;
            return Err(e);
        }
    };

    let sector_count = device.sector_count();
    let sector_size = device.sector_size();

    debug_print(&format!(
        "virtio-blk: device ready, {} sectors ({})",
        sector_count,
        sector_count * sector_size as u64
    ))?;

    // Wrap device in adapter that implements BlockDevice trait
    let adapter = VirtioBlkAdapter::new(device);

    // Try to mount ext2 filesystem
    let fs = match Ext2Fs::mount(&adapter) {
        Ok(fs) => {
            debug_print(&format!("virtio-blk: ext2 filesystem mounted"))?;
            Some(fs)
        }
        Err(e) => {
            debug_print(&format!("virtio-blk: no ext2 found ({:?}), raw block only", e))?;
            None
        }
    };

    // Initialize registry and create listen endpoint
    registry::init("blkdev")?;

    let listen_endpoint = info.tokens[SVC_TOKEN_LISTEN];
    let listen_endpoint = if listen_endpoint != 0 {
        listen_endpoint
    } else {
        endpoint_create(pci_token)?
    };

    registry::register_output("main", listen_endpoint)?;
    debug_print("virtio-blk: registered as blkdev:main")?;

    let registry_endpoint = registry::control_endpoint();

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

                // Handle request
                if let Some(ref fs) = fs {
                    handle_fs_request(fs, &adapter, msg, payload);
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

fn handle_fs_request(fs: &Ext2Fs, blk: &VirtioBlkAdapter, msg: &Message, payload: &[u8]) {
    let reply_token = extract_reply_token(msg);

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
            let len = msg.words[4].min(4096 - 64); // Cap at buffer size

            let mut read_buf = alloc::vec![0u8; len];
            match fs.read(inode, offset, &mut read_buf) {
                Ok(bytes_read) => {
                    let reply_msg = Message::new(FS_READ, [0, bytes_read, 0, 0, 0, 0], 2);
                    if let Some(token) = reply_token {
                        let _ = reply_with_payload(token, &reply_msg, &read_buf[..bytes_read]);
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
                    let reply_msg = Message::new(
                        FS_STAT,
                        [0, stat.size as usize, flags, 0, 0, 0],
                        3,
                    );
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

                            let reply_msg = Message::new(
                                FS_READDIR,
                                [0, entries.len(), 0, 0, 0, 0],
                                2,
                            );
                            if let Some(token) = reply_token {
                                let _ = reply_with_payload(token, &reply_msg, &data);
                            }
                        }
                        Err(_) => send_error_reply(reply_token, -1),
                    }
                }
                Err(_) => send_error_reply(reply_token, -3),
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
                send_error_reply(reply_token, -1);
                return;
            }

            let mut data_buf = alloc::vec![0u8; byte_count];
            match blk.read_bytes(start * 512, &mut data_buf) {
                Ok(bytes_read) => {
                    let reply_msg = Message::new(BLK_READ_LABEL, [bytes_read, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply_with_payload(token, &reply_msg, &data_buf);
                    }
                }
                Err(_) => send_error_reply(reply_token, -1),
            }
        }

        _ => {}
    }
}

fn handle_block_request(blk: &VirtioBlkAdapter, msg: &Message) {
    let reply_token = extract_reply_token(msg);

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
                send_error_reply(reply_token, -1);
                return;
            }

            let mut data_buf = alloc::vec![0u8; byte_count];
            match blk.read_bytes(start * 512, &mut data_buf) {
                Ok(bytes_read) => {
                    let reply_msg = Message::new(BLK_READ_LABEL, [bytes_read, 0, 0, 0, 0, 0], 1);
                    if let Some(token) = reply_token {
                        let _ = reply_with_payload(token, &reply_msg, &data_buf);
                    }
                }
                Err(_) => send_error_reply(reply_token, -1),
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
