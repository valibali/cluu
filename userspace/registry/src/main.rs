#![no_std]
#![no_main]

// Registry daemon: stores output metadata and brokers grant requests.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::mem::size_of;
use libcluu::boot::process_info;
use libcluu::ipc::send;
use libcluu::registry::{
    self, REGISTRY_GRANT_REQUEST_LABEL, REGISTRY_LIST_LABEL, REGISTRY_REGISTER_LABEL,
    REGISTRY_SUBSCRIBE_LABEL, REGISTRY_SUBSCRIBE_REPLY_LABEL, REGISTRY_UNREGISTER_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Error, Result};

const SVC_TOKEN_LISTEN: usize = 6;

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
    let endpoint = info.tokens[SVC_TOKEN_LISTEN];
    registry::init("registry")?;
    registry::register_default_outputs()?;

    debug_print("registry: ready")?;
    // Registry table: (service, endpoint) -> grant endpoint owned by producer.
    let mut entries: BTreeMap<(String, String), usize> = BTreeMap::new();
    // Pending subscriptions indexed by (service, endpoint) -> list of requester endpoints.
    // O(1) lookup instead of O(n) scan.
    let mut pending: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    let registry_control = registry::control_endpoint();
    let mut buf = [0u8; 256];
    // Main dispatch loop: handle register/unregister/list/subscribe requests.
    loop {
        let tokens = [endpoint, registry_control];
        // Wait for either registry API calls or a grant reply on our control endpoint.
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    if index == 1 {
                        let _ = registry::handle_incoming_message(&msg, payload);
                        continue;
                    }
                    match msg.tag.label {
                        REGISTRY_REGISTER_LABEL => {
                            // Producer registers an output: store metadata -> grant endpoint.
                            if let Some((service, endpoint)) = parse_names(payload) {
                                let grant_endpoint = msg.words[2];
                                if grant_endpoint != 0 {
                                    let key = (service.clone(), endpoint.clone());
                                    entries.insert(key.clone(), grant_endpoint);
                                    let _ = debug_print(&format!(
                                        "registry: registered {}:{}",
                                        service, endpoint
                                    ));
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
                                                    requester,
                                                    Error::InvalidArgument as i32,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            send_status(msg.words[1], 0)?;
                        }
                        REGISTRY_UNREGISTER_LABEL => {
                            // Producer unregisters an output; drop its metadata.
                            if let Some((service, endpoint)) = parse_names(payload) {
                                entries.remove(&(service.clone(), endpoint.clone()));
                                let _ = debug_print(&format!(
                                    "registry: unregistered {}:{}",
                                    service, endpoint
                                ));
                            }
                            send_status(msg.words[1], 0)?;
                        }
                        REGISTRY_LIST_LABEL => {
                            // List all registered outputs for discovery/debugging.
                            let reply_endpoint = msg.words[1];
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
                            let reply_endpoint = msg.words[1];
                            if let Some((service, endpoint)) = parse_names(payload) {
                                let _ = debug_print(&format!(
                                    "registry: subscribe {}:{} reply {} words {}",
                                    service, endpoint, reply_endpoint, msg.tag.words
                                ));
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
                                                reply_endpoint,
                                                Error::InvalidArgument as i32,
                                            )?;
                                        } else {
                                            send_status(reply_endpoint, 0)?;
                                        }
                                    }
                                    None => {
                                        // O(1) insert with dedup via contains check on small vec
                                        let requesters = pending.entry(key).or_default();
                                        if !requesters.contains(&reply_endpoint) {
                                            requesters.push(reply_endpoint);
                                        }
                                        // Accept the request and grant when the output appears.
                                        send_status(reply_endpoint, 0)?;
                                    }
                                }
                            } else {
                                send_status(reply_endpoint, Error::InvalidArgument as i32)?;
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

fn parse_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    // Messages are encoded as a header followed by payload bytes.
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

fn make_payload_message(label: u32, payload_len: usize, words: &[usize]) -> Message {
    let mut msg = Message::new(label, [0; 6], 1);
    msg.words[0] = payload_len;
    let mut count = 1;
    for (idx, word) in words.iter().enumerate() {
        if idx + 1 >= msg.words.len() {
            break;
        }
        msg.words[idx + 1] = *word;
        count += 1;
    }
    msg.tag.words = count as u8;
    msg
}

fn send_with_payload(endpoint: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    libcluu::syscall::ipc_send(endpoint, &buffer)
}

fn send_status(reply_endpoint: usize, code: i32) -> Result<()> {
    if reply_endpoint == 0 {
        return Ok(());
    }
    let reply = Message::new(
        REGISTRY_SUBSCRIBE_REPLY_LABEL,
        [0, code as usize, 0, 0, 0, 0],
        2,
    );
    send(reply_endpoint, &reply, IpcFlags::empty())
}
