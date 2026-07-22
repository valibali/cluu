//! IPC handlers for drivermon — one function per supervision label.
//!
//! Wire format is documented at the label constants in
//! `userspace/libcluu/src/ipc.rs` (DRIVERMON_REGISTER_LABEL etc.).

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::runtime_table::{DriverRuntimeTable, RestartPolicy};
use libcluu::ipc::{
    reply_to_sender, DRIVERMON_REBIND_LABEL, DRIVERMON_REGISTER_LABEL,
    DRIVERMON_RESPAWN_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::debug_print;

use crate::supervision;

const REPLY_OK: usize = 0;
const REPLY_NOT_FOUND: usize = 1;
const REPLY_INVALID_ARG: usize = 2;

const POLICY_ALWAYS: usize = 0;
const POLICY_NEVER: usize = 1;
const POLICY_ON_FAULT: usize = 2;

fn policy_from_word(raw: usize) -> Option<RestartPolicy> {
    match raw {
        POLICY_ALWAYS => Some(RestartPolicy::Always),
        POLICY_NEVER => Some(RestartPolicy::Never),
        POLICY_ON_FAULT => Some(RestartPolicy::OnFault),
        _ => None,
    }
}

/// Split a NUL-separated payload into leading non-empty segments.
/// Returns up to `max` segments; ignores trailing empty segments.
fn split_payload(payload: &[u8], max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..payload.len() {
        if payload[i] == 0 {
            if i > start {
                if let Ok(s) = core::str::from_utf8(&payload[start..i]) {
                    out.push(String::from(s));
                    if out.len() == max {
                        return out;
                    }
                }
            }
            start = i + 1;
        }
    }
    if start < payload.len() {
        if let Ok(s) = core::str::from_utf8(&payload[start..]) {
            if !s.is_empty() {
                out.push(String::from(s));
            }
        }
    }
    out
}

pub fn handle_register(table: &mut DriverRuntimeTable, msg: &Message, payload: &[u8], ep: usize) {
    let pid = msg.words[1] as u32;
    let policy = match policy_from_word(msg.words[2]) {
        Some(p) => p,
        None => {
            let _ = debug_print("drivermon: REGISTER invalid policy");
            let reply = Message::new(
                DRIVERMON_REGISTER_LABEL,
                [REPLY_INVALID_ARG, 0, 0, 0, 0, 0],
                1,
            );
            let _ = reply_to_sender(msg, &reply, ep, IpcFlags::empty());
            return;
        }
    };
    let has_fallback = msg.words[3] != 0;

    let parts = split_payload(payload, if has_fallback { 3 } else { 2 });
    if parts.len() < 2 {
        let _ = debug_print("drivermon: REGISTER missing device_path/driver_image");
        let reply = Message::new(
            DRIVERMON_REGISTER_LABEL,
            [REPLY_INVALID_ARG, 0, 0, 0, 0, 0],
            1,
        );
        let _ = reply_to_sender(msg, &reply, ep, IpcFlags::empty());
        return;
    }
    let device_path = parts[0].clone();
    let device_path_for_notify = parts[0].clone();
    let driver_image = parts[1].clone();
    let fallback = if has_fallback && parts.len() >= 3 {
        Some(parts[2].clone())
    } else {
        None
    };

    table.register(pid, device_path, driver_image, policy, fallback);

    let _ = debug_print(&alloc::format!("drivermon: REGISTER pid={}", pid));
    let reply = Message::new(DRIVERMON_REGISTER_LABEL, [REPLY_OK, 0, 0, 0, 0, 0], 1);
    let _ = reply_to_sender(msg, &reply, ep, IpcFlags::empty());

    supervision::send_device_state(&device_path_for_notify, supervision::DEVICE_STATE_BOUND);
}

pub fn handle_respawn(table: &mut DriverRuntimeTable, msg: &Message, payload: &[u8], ep: usize) {
    let path = match core::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => {
            let reply = Message::new(
                DRIVERMON_RESPAWN_LABEL,
                [REPLY_INVALID_ARG, 0, 0, 0, 0, 0],
                1,
            );
            let _ = reply_to_sender(msg, &reply, ep, IpcFlags::empty());
            return;
        }
    };

    let (status, count) = match table.respawn(path) {
        Some(entry) => (REPLY_OK, entry.restart_count as usize),
        None => (REPLY_NOT_FOUND, 0),
    };

    let _ = debug_print(&alloc::format!(
        "drivermon: RESPAWN {} status={}",
        path, status
    ));
    let reply = Message::new(DRIVERMON_RESPAWN_LABEL, [status, count, 0, 0, 0, 0], 2);
    let _ = reply_to_sender(msg, &reply, ep, IpcFlags::empty());
}

pub fn handle_rebind(table: &mut DriverRuntimeTable, msg: &Message, payload: &[u8], ep: usize) {
    let parts = split_payload(payload, 2);
    if parts.len() < 2 {
        let reply = Message::new(
            DRIVERMON_REBIND_LABEL,
            [REPLY_INVALID_ARG, 0, 0, 0, 0, 0],
            1,
        );
        let _ = reply_to_sender(msg, &reply, ep, IpcFlags::empty());
        return;
    }
    let device_path = parts[0].clone();
    let new_driver_image = parts[1].clone();

    let (status, old_pid) = match table.rebind(&device_path, new_driver_image) {
        Some(pid) => (REPLY_OK, pid as usize),
        None => (REPLY_NOT_FOUND, 0),
    };

    let _ = debug_print(&alloc::format!(
        "drivermon: REBIND {} status={} old_pid={}",
        device_path, status, old_pid
    ));
    let reply = Message::new(DRIVERMON_REBIND_LABEL, [status, old_pid, 0, 0, 0, 0], 2);
    let _ = reply_to_sender(msg, &reply, ep, IpcFlags::empty());
}
