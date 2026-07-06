//! IPC handlers for devmgr — one function per label.
//!
//! Each handler takes `&mut DevRegistry` + the inbound `Message` and replies
//! via `reply_to_sender*` (handles both sync `ipc_call` and async
//! `IpcCallFuture` callers — see KB gotcha `cluu-pts-verb-async-reply-tag-
//! silent-drop`).

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::device::{DeviceClass, DeviceId};
use crate::dev_registry::DevRegistry;
use libcluu::ipc::{
    reply_to_sender, reply_to_sender_with_payload,
    DEVMGR_GRANT_DEVICE_LABEL, DEVMGR_GRANT_REGION_LABEL, DEVMGR_LIST_FOR_ENVELOPE_LABEL,
    DEVMGR_REGISTER_CHAR_LABEL, DEVMGR_REGISTER_LABEL, DEVMGR_REVOKE_LABEL,
};
use libcluu::rights::Rights;
use libcluu::syscall::{token_derive_scoped_block_region, token_derive_scoped_device_region, token_revoke};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Error};

const REPLY_OK: usize = 0;

fn err_to_status(e: Error) -> usize {
    e as i32 as usize
}

pub fn handle_register_block(
    registry: &mut DevRegistry,
    msg: &Message,
    fallback_ep: usize,
    boot_root_block_token: usize,
) {
    let device_id = msg.words[0] as DeviceId;
    let total_sectors = msg.words[1] as u64;

    let root_token = if device_id == 0 {
        boot_root_block_token
    } else {
        0
    };

    let path = if msg.words[2] != 0 {
        allocate_path_from_payload(msg)
    } else {
        allocate_block_path(device_id)
    };

    registry.register_block(device_id, path, 0, root_token, total_sectors);

    let _ = debug_print("devmgr: registered block device");
    let reply_msg = Message::new(DEVMGR_REGISTER_LABEL, [REPLY_OK, 0, 0, 0, 0, 0], 1);
    let _ = reply_to_sender(msg, &reply_msg, fallback_ep, IpcFlags::empty());
}

pub fn handle_register_char(
    registry: &mut DevRegistry,
    msg: &Message,
    fallback_ep: usize,
    root_device_token: usize,
) {
    let class_raw = msg.words[0] as u8;
    let class = match DeviceClass::from_u8(class_raw) {
        Some(c) => c,
        None => {
            let reply_msg = Message::new(
                DEVMGR_REGISTER_CHAR_LABEL,
                [Error::InvalidArgument as i32 as usize, 0, 0, 0, 0, 0],
                1,
            );
            let _ = reply_to_sender(msg, &reply_msg, fallback_ep, IpcFlags::empty());
            return;
        }
    };
    let driver_endpoint = msg.words[1];

    let path = allocate_path_from_payload(msg);

    let id = registry.register_char(class, path, driver_endpoint, root_device_token);

    let _ = debug_print("devmgr: registered char device");
    let reply_msg =
        Message::new(DEVMGR_REGISTER_CHAR_LABEL, [REPLY_OK, id as usize, 0, 0, 0, 0], 1);
    let _ = reply_to_sender(msg, &reply_msg, fallback_ep, IpcFlags::empty());
}

pub fn handle_grant_region(
    registry: &DevRegistry,
    msg: &Message,
    fallback_ep: usize,
) {
    let device_id = msg.words[0] as DeviceId;
    let start_sector = msg.words[1] as u64;
    let sector_count_arg = msg.words[2] as u64;

    let (status, token_handle) = match registry.get(device_id) {
        Some(dev) => {
            if dev.root_token == 0 {
                (Error::NotFound as i32 as usize, 0usize)
            } else {
                let sector_count = if sector_count_arg == 0 {
                    dev.total_sectors
                } else {
                    sector_count_arg
                };
                let rights = (Rights::READ | Rights::WRITE).bits() as u32;
                match token_derive_scoped_block_region(
                    dev.root_token,
                    rights,
                    0,
                    start_sector,
                    sector_count,
                ) {
                    Ok(handle) => (REPLY_OK, handle),
                    Err(e) => (err_to_status(e), 0usize),
                }
            }
        }
        None => (Error::NotFound as i32 as usize, 0usize),
    };

    let reply_msg = Message::new(
        DEVMGR_GRANT_REGION_LABEL,
        [status, token_handle, 0, 0, 0, 0],
        2,
    );
    let _ = reply_to_sender(msg, &reply_msg, fallback_ep, IpcFlags::empty());
}

pub fn handle_grant_device(
    registry: &DevRegistry,
    msg: &Message,
    fallback_ep: usize,
) {
    let device_id = msg.words[0] as DeviceId;
    let requested_rights = msg.words[1] as u32;
    let child_base = msg.words[2] as u64;
    let child_len = msg.words[3] as u64;

    let (status, token_handle) = match registry.get(device_id) {
        Some(dev) => {
            if dev.root_token == 0 {
                (Error::NotFound as i32 as usize, 0usize)
            } else {
                match token_derive_scoped_device_region(
                    dev.root_token,
                    requested_rights,
                    0,
                    child_base,
                    child_len,
                ) {
                    Ok(handle) => (REPLY_OK, handle),
                    Err(e) => (err_to_status(e), 0usize),
                }
            }
        }
        None => (Error::NotFound as i32 as usize, 0usize),
    };

    let reply_msg = Message::new(
        DEVMGR_GRANT_DEVICE_LABEL,
        [status, token_handle, 0, 0, 0, 0],
        2,
    );
    let _ = reply_to_sender(msg, &reply_msg, fallback_ep, IpcFlags::empty());
}

pub fn handle_revoke(msg: &Message, fallback_ep: usize) {
    let token_handle = msg.words[0];
    let status = match token_revoke(token_handle) {
        Ok(_) => REPLY_OK,
        Err(e) => err_to_status(e),
    };
    let reply_msg = Message::new(DEVMGR_REVOKE_LABEL, [status, 0, 0, 0, 0, 0], 1);
    let _ = reply_to_sender(msg, &reply_msg, fallback_ep, IpcFlags::empty());
}

pub fn handle_list_for_envelope(
    registry: &DevRegistry,
    msg: &Message,
    payload: &[u8],
    fallback_ep: usize,
) {
    let is_root = msg.words[0] != 0;

    let cluufile_devices = parse_device_decls(payload);

    let visible = registry.list_for_envelope(is_root, &cluufile_devices);

    let serialized = serialize_visible(&visible);

    let reply_msg = Message::new(
        DEVMGR_LIST_FOR_ENVELOPE_LABEL,
        [visible.len(), 0, 0, 0, 0, 0],
        1,
    );
    let _ = reply_to_sender_with_payload(msg, &reply_msg, &serialized, fallback_ep);
}

fn allocate_block_path(device_id: DeviceId) -> String {
    let mut s = String::from("/dev/disk/");
    let mut digits = String::new();
    let mut n = device_id;
    if n == 0 {
        digits.push('0');
    }
    while n > 0 {
        let d = (n % 10) as u8;
        digits.insert(0, (b'0' + d) as char);
        n /= 10;
    }
    s.push_str(&digits);
    s
}

fn allocate_path_from_payload(msg: &Message) -> String {
    let payload_len = msg.words[3] as usize;
    if payload_len == 0 || payload_len > 128 {
        return String::from("/dev/unknown");
    }
    let msg_size = core::mem::size_of::<Message>();
    let start = msg_size;
    let end = start + payload_len;
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (msg as *const Message as *const u8).add(start),
            payload_len.min(end - start),
        )
    };
    String::from_utf8_lossy(bytes).into_owned()
}

fn parse_device_decls(payload: &[u8]) -> Vec<String> {
    let mut decls = Vec::new();
    let mut start = 0;
    for i in 0..payload.len() {
        if payload[i] == b'\n' || payload[i] == 0 {
            if i > start {
                if let Ok(s) = core::str::from_utf8(&payload[start..i]) {
                    decls.push(String::from(s));
                }
            }
            start = i + 1;
        }
    }
    if start < payload.len() {
        if let Ok(s) = core::str::from_utf8(&payload[start..]) {
            if !s.is_empty() {
                decls.push(String::from(s));
            }
        }
    }
    decls
}

fn serialize_visible(visible: &[crate::dev_registry::VisibleDevice]) -> Vec<u8> {
    let mut buf = Vec::new();
    for dev in visible {
        let path_bytes = dev.path.as_bytes();
        buf.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(path_bytes);
        buf.extend_from_slice(&dev.device_id.to_le_bytes());
        buf.extend_from_slice(&(dev.class as u8).to_le_bytes());
        buf.extend_from_slice(&dev.rights.to_le_bytes());
    }
    buf
}
