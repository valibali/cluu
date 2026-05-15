#![no_std]
#![no_main]

// Registry daemon: stores output metadata and brokers grant requests.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_EXTRA_0};
use libcluu::ipc::{extract_reply_id, make_payload_message, parse_message, reply, send};
use libcluu::registry::{
    self, REGISTRY_GRANT_REQUEST_LABEL, REGISTRY_LIST_LABEL, REGISTRY_REGISTER_LABEL,
    REGISTRY_SUBSCRIBE_LABEL, REGISTRY_SUBSCRIBE_REPLY_LABEL, REGISTRY_UNREGISTER_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Error, Result};

// PendingSubscription struct removed - now using indexed BTreeMap instead.

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    let info = process_info();
    let endpoint = info.tokens[TOKEN_EXTRA_0];
    registry::init("registry")?;
    registry::register_default_outputs()?;

    debug_print("registry: ready")?;
    // Registry table: (service, endpoint) -> grant endpoint owned by producer.
    let mut entries: BTreeMap<(String, String), usize> = BTreeMap::new();
    // Pending subscriptions indexed by (service, endpoint) -> list of requester endpoints.
    // O(1) lookup instead of O(n) scan.
    let mut pending: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    // Track producer ownership for each output entry.
    let mut entry_owner: BTreeMap<(String, String), usize> = BTreeMap::new();
    // Bind sender thread id -> latest control endpoint for async notifications.
    let mut sender_control_endpoint: BTreeMap<usize, usize> = BTreeMap::new();
    let registry_control = registry::control_endpoint();
    let mut buf = [0u8; 256];
    // Main dispatch loop: handle register/unregister/list/subscribe requests.
    loop {
        let tokens = [endpoint, registry_control];
        // Wait for either registry API calls or a grant reply on our control endpoint.
        match libcluu::syscall::ipc_recv_any_with_sender(&tokens, &mut buf, u64::MAX) {
            Ok((index, len, sender_tid)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    if index == 1 {
                        let _ = registry::handle_incoming_message(&msg, payload);
                        continue;
                    }
                    let reply_token = extract_reply_id(&msg);
                    match msg.tag.label {
                        REGISTRY_REGISTER_LABEL => {
                            // Producer registers an output: store metadata -> grant endpoint.
                            if let Some((service, endpoint)) = parse_names(payload) {
                                let grant_endpoint =
                                    if msg.tag.words >= 3 { msg.words[2] } else { 0 };
                                if grant_endpoint != 0 {
                                    let key = (service.clone(), endpoint.clone());
                                    if sender_tid == 0 {
                                        let _ = debug_print(&format!(
                                            "registry: deny register {}:{} sender_tid=0",
                                            service, endpoint
                                        ));
                                        send_status(
                                            reply_token,
                                            msg.words[1],
                                            Error::PermissionDenied as i32,
                                        )?;
                                        continue;
                                    }
                                    if let Some(owner_tid) = entry_owner.get(&key).copied() {
                                        if owner_tid != sender_tid {
                                            let _ = debug_print(&format!(
                                                "registry: deny register {}:{} sender_tid={} owner_tid={}",
                                                service, endpoint, sender_tid, owner_tid
                                            ));
                                            send_status(
                                                reply_token,
                                                msg.words[1],
                                                Error::PermissionDenied as i32,
                                            )?;
                                            continue;
                                        }
                                    }
                                    entries.insert(key.clone(), grant_endpoint);
                                    entry_owner.insert(key.clone(), sender_tid);
                                    // O(1) lookup + O(k) drain where k = pending for THIS output
                                    if let Some(requesters) = pending.remove(&key) {
                                        let grant_payload = encode_single_name(&endpoint);
                                        for requester in requesters {
                                            let grant_msg = make_payload_message(
                                                REGISTRY_GRANT_REQUEST_LABEL,
                                                grant_payload.len(),
                                                &[requester],
                                            );
                                            if let Err(_err) = send_with_payload(
                                                grant_endpoint,
                                                &grant_msg,
                                                &grant_payload,
                                            ) {
                                                let _ = send_status(
                                                    None,
                                                    requester,
                                                    Error::InvalidArgument as i32,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            send_status(reply_token, msg.words[1], 0)?;
                        }
                        REGISTRY_UNREGISTER_LABEL => {
                            // Producer unregisters an output; drop its metadata.
                            if let Some((service, endpoint)) = parse_names(payload) {
                                let key = (service.clone(), endpoint.clone());
                                if sender_tid == 0 {
                                    let _ = debug_print(&format!(
                                        "registry: deny unregister {}:{} sender_tid=0",
                                        service, endpoint
                                    ));
                                    send_status(
                                        reply_token,
                                        msg.words[1],
                                        Error::PermissionDenied as i32,
                                    )?;
                                    continue;
                                }
                                if let Some(owner_tid) = entry_owner.get(&key).copied() {
                                    if owner_tid != sender_tid {
                                        let _ = debug_print(&format!(
                                            "registry: deny unregister {}:{} sender_tid={} owner_tid={}",
                                            service, endpoint, sender_tid, owner_tid
                                        ));
                                        send_status(
                                            reply_token,
                                            msg.words[1],
                                            Error::PermissionDenied as i32,
                                        )?;
                                        continue;
                                    }
                                    entry_owner.remove(&key);
                                    entries.remove(&key);
                                }
                                let _ = debug_print(&format!(
                                    "registry: unregistered {}:{}",
                                    service, endpoint
                                ));
                            }
                            send_status(reply_token, msg.words[1], 0)?;
                        }
                        REGISTRY_LIST_LABEL => {
                            // List all registered outputs for discovery/debugging.
                            let reply_endpoint = match resolve_sender_control_endpoint(
                                &mut sender_control_endpoint,
                                sender_tid,
                                if msg.tag.words >= 2 { msg.words[1] } else { 0 },
                            ) {
                                Ok(endpoint) => endpoint,
                                Err(err) => {
                                    let _ = send_status(
                                        reply_token,
                                        if msg.tag.words >= 2 { msg.words[1] } else { 0 },
                                        err as i32,
                                    );
                                    continue;
                                }
                            };
                            if reply_endpoint != 0 {
                                let payload = encode_entries(&entries);
                                let reply = make_payload_message(
                                    REGISTRY_SUBSCRIBE_REPLY_LABEL,
                                    payload.len(),
                                    &[0],
                                );
                                let _ = send_with_payload(reply_endpoint, &reply, &payload);
                            }
                        }
                        REGISTRY_SUBSCRIBE_LABEL => {
                            // Subscriber requests an output; forward a grant request to producer.
                            let reply_endpoint = match resolve_sender_control_endpoint(
                                &mut sender_control_endpoint,
                                sender_tid,
                                if msg.tag.words >= 2 { msg.words[1] } else { 0 },
                            ) {
                                Ok(endpoint) => endpoint,
                                Err(err) => {
                                    send_status(
                                        reply_token,
                                        if msg.tag.words >= 2 { msg.words[1] } else { 0 },
                                        err as i32,
                                    )?;
                                    continue;
                                }
                            };
                            if reply_endpoint == 0 {
                                send_status(
                                    reply_token,
                                    reply_endpoint,
                                    Error::InvalidArgument as i32,
                                )?;
                                continue;
                            }
                            if let Some((service, endpoint)) = parse_names(payload) {
                                let key = (service.clone(), endpoint.clone());
                                match entries.get(&key) {
                                    Some(grant_endpoint) => {
                                        let grant_payload = encode_single_name(&endpoint);
                                        let grant_msg = make_payload_message(
                                            REGISTRY_GRANT_REQUEST_LABEL,
                                            grant_payload.len(),
                                            &[reply_endpoint],
                                        );
                                        if let Err(_err) = send_with_payload(
                                            *grant_endpoint,
                                            &grant_msg,
                                            &grant_payload,
                                        ) {
                                            send_status(
                                                reply_token,
                                                reply_endpoint,
                                                Error::InvalidArgument as i32,
                                            )?;
                                        } else {
                                            send_status(reply_token, reply_endpoint, 0)?;
                                        }
                                    }
                                    None => {
                                        // O(1) insert with dedup via contains check on small vec
                                        let requesters = pending.entry(key).or_default();
                                        if !requesters.contains(&reply_endpoint) {
                                            requesters.push(reply_endpoint);
                                        }
                                        // Accept the request and grant when the output appears.
                                        send_status(reply_token, reply_endpoint, 0)?;
                                    }
                                }
                            } else {
                                send_status(
                                    reply_token,
                                    reply_endpoint,
                                    Error::InvalidArgument as i32,
                                )?;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(Error::WouldBlock) => {
                let _ = yield_cpu();
            }
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    }
}

fn resolve_sender_control_endpoint(
    sender_control_endpoint: &mut BTreeMap<usize, usize>,
    sender_tid: usize,
    requested_endpoint: usize,
) -> Result<usize> {
    if sender_tid == 0 {
        return Err(Error::PermissionDenied);
    }
    if requested_endpoint != 0 {
        sender_control_endpoint.insert(sender_tid, requested_endpoint);
        return Ok(requested_endpoint);
    }
    Ok(sender_control_endpoint
        .get(&sender_tid)
        .copied()
        .unwrap_or(0))
}


fn parse_names(payload: &[u8]) -> Option<(String, String)> {
    // Payload encoding matches registry::encode_names (len/len/bytes/bytes).
    if payload.len() < 4 {
        return None;
    }
    let service_len = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    let endpoint_len = u16::from_le_bytes([payload[2], payload[3]]) as usize;
    let start = 4;
    let mid = start + service_len;
    let end = mid + endpoint_len;
    if end > payload.len() {
        return None;
    }
    let service = core::str::from_utf8(&payload[start..mid]).ok()?.to_string();
    let endpoint = core::str::from_utf8(&payload[mid..end]).ok()?.to_string();
    Some((service, endpoint))
}

fn encode_entries(entries: &BTreeMap<(String, String), usize>) -> Vec<u8> {
    let mut payload = Vec::new();
    for ((service, endpoint), _) in entries.iter() {
        let entry = encode_names(service, endpoint);
        payload.extend_from_slice(&entry);
    }
    payload
}

fn encode_names(service: &str, endpoint: &str) -> Vec<u8> {
    let service_bytes = service.as_bytes();
    let endpoint_bytes = endpoint.as_bytes();
    let mut payload = Vec::with_capacity(4 + service_bytes.len() + endpoint_bytes.len());
    payload.extend_from_slice(&(service_bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(endpoint_bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(service_bytes);
    payload.extend_from_slice(endpoint_bytes);
    payload
}

fn encode_single_name(name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let mut payload = Vec::with_capacity(4 + bytes.len());
    payload.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(&0u16.to_le_bytes());
    payload.extend_from_slice(bytes);
    payload
}

fn send_with_payload(endpoint: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    libcluu::syscall::ipc_send(endpoint, &buffer)
}

fn send_status(reply_token: Option<usize>, reply_endpoint: usize, code: i32) -> Result<()> {
    let status = Message::new(
        REGISTRY_SUBSCRIBE_REPLY_LABEL,
        [0, code as usize, 0, 0, 0, 0],
        2,
    );
    if let Some(token) = reply_token {
        return reply(token, &status, IpcFlags::empty());
    }
    if reply_endpoint == 0 {
        return Ok(());
    }
    send(reply_endpoint, &status, IpcFlags::empty())
}
