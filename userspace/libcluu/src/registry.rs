//! Userspace registry client + producer grant handling.
//!
//! This module lets a process:
//! - register outputs it owns (metadata only),
//! - subscribe to outputs from other services via the registry,
//! - handle grant requests to mint send-only tokens for subscribers.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

use crate::boot::{process_info, TOKEN_IPC, TOKEN_REGISTRY};
use crate::rights::Rights;
use crate::syscall::{endpoint_create, ipc_recv_any, ipc_recv_nonblocking, token_derive};
use crate::types::Message;
use crate::{Error, Result};

pub const REGISTRY_REGISTER_LABEL: u32 = 0x100;
pub const REGISTRY_UNREGISTER_LABEL: u32 = 0x101;
pub const REGISTRY_LIST_LABEL: u32 = 0x102;
pub const REGISTRY_SUBSCRIBE_LABEL: u32 = 0x103;
pub const REGISTRY_SUBSCRIBE_REPLY_LABEL: u32 = 0x104;
pub const REGISTRY_GRANT_REQUEST_LABEL: u32 = 0x105;
pub const REGISTRY_GRANT_DELIVER_LABEL: u32 = 0x106;
pub enum RegistryEvent {
    /// Producer granted a token for the named output.
    Grant { service_name: String, name: String, token: usize },
    /// Registry replied to a subscribe request; 0 is OK.
    SubscribeStatus { code: i32 },
}

#[derive(Default)]
struct RegistryState {
    /// Service name used as the registry namespace.
    service_name: Option<String>,
    /// Registry endpoint token (send-only).
    registry_endpoint: usize,
    /// Per-process control endpoint (recv-only).
    control_endpoint: usize,
    /// Outputs owned by this process: name -> endpoint token.
    outputs: BTreeMap<String, usize>,
    /// Service lookup cache: "service:endpoint" -> granted token.
    lookup_cache: BTreeMap<String, usize>,
}

lazy_static::lazy_static! {
    // Global registry client state for this process.
    static ref REGISTRY_STATE: Mutex<RegistryState> = Mutex::new(RegistryState::default());
}

/// Initialize the registry client for this process and allocate a control endpoint.
pub fn init(service_name: &str) -> Result<()> {
    let info = process_info();
    let registry_endpoint = info.tokens[TOKEN_REGISTRY];
    let ipc_cap = info.tokens[TOKEN_IPC];
    if registry_endpoint == 0 || ipc_cap == 0 {
        return Err(Error::InvalidArgument);
    }
    init_with_tokens(service_name, registry_endpoint, ipc_cap)
}

/// Initialize the registry client with explicit tokens.
///
/// This is used by init, which creates the registry endpoint itself and
/// therefore does not have TOKEN_REGISTRY set in its boot parameters.
pub fn init_with_tokens(
    service_name: &str,
    registry_endpoint: usize,
    ipc_cap: usize,
) -> Result<()> {
    if registry_endpoint == 0 || ipc_cap == 0 {
        return Err(Error::InvalidArgument);
    }

    // The control endpoint is where subscribe replies and grants arrive.
    let control_endpoint = endpoint_create(ipc_cap)?;
    let mut state = REGISTRY_STATE.lock();
    state.service_name = Some(service_name.to_string());
    state.registry_endpoint = registry_endpoint;
    state.control_endpoint = control_endpoint;
    Ok(())
}

pub fn control_endpoint() -> usize {
    REGISTRY_STATE.lock().control_endpoint
}

/// Look up a service by its full name (e.g., "vfs:main", "procmgr:spawn").
///
/// This is a convenience function that subscribes to the service and returns
/// the granted endpoint token. The service name should be in "service:output" format.
///
/// # Returns
/// - `Some(token)`: Endpoint token for the service
/// - `None`: Service not found or lookup failed
pub fn lookup_service(full_name: &str) -> Option<usize> {
    if full_name == "vfs:main" {
        let info = process_info();
        let session_vfs_ep = info.params[crate::boot::PARAM_SESSION_VFS_EP] as usize;
        if session_vfs_ep != 0 {
            return Some(session_vfs_ep);
        }
    }

    {
        let state = REGISTRY_STATE.lock();
        if let Some(token) = state.lookup_cache.get(full_name) {
            return Some(*token);
        }
    }

    // Parse "service:output" format
    let parts: Vec<&str> = full_name.splitn(2, ':').collect();
    if parts.len() != 2 {
        return None;
    }
    let service_name = parts[0];
    let endpoint_name = parts[1];

    // Subscribe and get the token
    let token = subscribe_output(service_name, endpoint_name).ok()?;
    let mut state = REGISTRY_STATE.lock();
    state.lookup_cache.insert(full_name.to_string(), token);
    Some(token)
}

/// Register an output endpoint with the registry.
///
/// This is a synchronous call: it blocks until the registry has processed the
/// registration and replied. Callers can rely on the entry being committed to
/// the registry before this function returns.
pub fn register_output(endpoint_name: &str, endpoint_token: usize) -> Result<()> {
    let (service_name, registry_endpoint, control_endpoint) = {
        let mut state = REGISTRY_STATE.lock();
        let service_name = state.service_name.clone().ok_or(Error::InvalidArgument)?;
        let registry_endpoint = state.registry_endpoint;
        let control_endpoint = state.control_endpoint;
        if registry_endpoint == 0 || control_endpoint == 0 {
            let _ = crate::debug_print(&alloc::format!(
                "registry-client: register_output '{}:{}' SKIPPED: registry_endpoint={} control_endpoint={}",
                state.service_name.as_deref().unwrap_or("?"),
                endpoint_name,
                registry_endpoint,
                control_endpoint,
            ));
            return Err(Error::InvalidArgument);
        }
        // Keep a local map so we can grant later.
        state
            .outputs
            .insert(endpoint_name.to_string(), endpoint_token);
        (service_name, registry_endpoint, control_endpoint)
    };
    // Lock is released before the blocking IPC call.
    let payload = encode_names(&service_name, endpoint_name);
    let msg = make_payload_message(
        REGISTRY_REGISTER_LABEL,
        payload.len(),
        &[control_endpoint, control_endpoint],
    );
    let result = call_and_check(registry_endpoint, &msg, &payload);
    if let Err(ref e) = result {
        let _ = crate::debug_print(&alloc::format!(
            "registry-client: register_output '{}:{}' FAILED: {:?}",
            service_name, endpoint_name, e,
        ));
    }
    result
}

pub fn unregister_output(endpoint_name: &str) -> Result<()> {
    let (service_name, registry_endpoint, control_endpoint) = {
        let mut state = REGISTRY_STATE.lock();
        let service_name = state.service_name.clone().ok_or(Error::InvalidArgument)?;
        let registry_endpoint = state.registry_endpoint;
        let control_endpoint = state.control_endpoint;
        if registry_endpoint == 0 || control_endpoint == 0 {
            return Err(Error::InvalidArgument);
        }
        // Stop granting this output.
        state.outputs.remove(endpoint_name);
        (service_name, registry_endpoint, control_endpoint)
    };
    // Lock is released before the blocking IPC call.
    let payload = encode_names(&service_name, endpoint_name);
    let msg = make_payload_message(
        REGISTRY_UNREGISTER_LABEL,
        payload.len(),
        &[control_endpoint],
    );
    call_and_check(registry_endpoint, &msg, &payload)
}

/// Unregister an output published by another service (admin/force path).
///
/// Used by procmgr to clear a killed service's registry entries before spawning
/// a replacement, so that the new service can register under the same names.
/// The registry allows this since 2026-05-15 (force-unregister).
pub fn force_unregister_service_output(
    service_name: &str,
    endpoint_name: &str,
) -> Result<()> {
    let (registry_endpoint, control_endpoint) = {
        let state = REGISTRY_STATE.lock();
        (state.registry_endpoint, state.control_endpoint)
    };
    if registry_endpoint == 0 || control_endpoint == 0 {
        return Err(Error::InvalidArgument);
    }
    let payload = encode_names(service_name, endpoint_name);
    let msg = make_payload_message(REGISTRY_UNREGISTER_LABEL, payload.len(), &[control_endpoint]);
    call_and_check(registry_endpoint, &msg, &payload)
}

/// Register standard outputs (stdin/stdout/stderr/stdlog) when present.
///
/// Uses a fire-and-forget send (not a blocking call) so that it is safe to
/// call from the registry process itself before it enters its receive loop.
/// For all other callers the difference is invisible — the registry processes
/// the message quickly and no ACK is needed for default-output registration.
pub fn register_default_outputs() -> Result<()> {
    let info = process_info();
    let stdin = info.tokens[crate::boot::TOKEN_STDIN];
    let stdout = info.tokens[crate::boot::TOKEN_STDOUT];
    let stderr = info.tokens[crate::boot::TOKEN_STDERR];
    let stdlog = info.tokens[crate::boot::TOKEN_STDLOG];

    if stdin != 0 {
        let _ = register_output_async("stdin", stdin);
    }
    if stdout != 0 {
        let _ = register_output_async("stdout", stdout);
    }
    if stderr != 0 {
        let _ = register_output_async("stderr", stderr);
    }
    if stdlog != 0 {
        let _ = register_output_async("stdlog", stdlog);
    }
    Ok(())
}

/// Fire-and-forget register: sends REGISTRY_REGISTER_LABEL without waiting for ACK.
///
/// Only used internally by `register_default_outputs` where blocking is not safe
/// (e.g., when called from the registry process before its receive loop starts).
fn register_output_async(endpoint_name: &str, endpoint_token: usize) -> Result<()> {
    let (service_name, registry_endpoint, control_endpoint) = {
        let mut state = REGISTRY_STATE.lock();
        let service_name = state.service_name.clone().ok_or(Error::InvalidArgument)?;
        let registry_endpoint = state.registry_endpoint;
        let control_endpoint = state.control_endpoint;
        if registry_endpoint == 0 || control_endpoint == 0 {
            return Err(Error::InvalidArgument);
        }
        state
            .outputs
            .insert(endpoint_name.to_string(), endpoint_token);
        (service_name, registry_endpoint, control_endpoint)
    };
    let payload = encode_names(&service_name, endpoint_name);
    let msg = make_payload_message(
        REGISTRY_REGISTER_LABEL,
        payload.len(),
        &[control_endpoint, control_endpoint],
    );
    send_with_payload(registry_endpoint, &msg, &payload)
}

pub fn subscribe_output(service_name: &str, endpoint_name: &str) -> Result<usize> {
    if service_name == "vfs" && endpoint_name == "main" {
        let info = process_info();
        let session_vfs_ep = info.params[crate::boot::PARAM_SESSION_VFS_EP] as usize;
        if session_vfs_ep != 0 {
            return Ok(session_vfs_ep);
        }
    }

    let (registry_endpoint, control_endpoint) = {
        let state = REGISTRY_STATE.lock();
        (state.registry_endpoint, state.control_endpoint)
    };
    if registry_endpoint == 0 || control_endpoint == 0 {
        return Err(Error::InvalidArgument);
    }

    let payload = encode_names(service_name, endpoint_name);
    let msg = make_payload_message(REGISTRY_SUBSCRIBE_LABEL, payload.len(), &[control_endpoint]);
    // Ask the registry to broker a grant request to the producer.
    send_with_payload(registry_endpoint, &msg, &payload)?;

    wait_for_grant(service_name, endpoint_name)
}

pub fn request_subscription(service_name: &str, endpoint_name: &str) -> Result<()> {
    let (registry_endpoint, control_endpoint) = {
        let state = REGISTRY_STATE.lock();
        (state.registry_endpoint, state.control_endpoint)
    };
    if registry_endpoint == 0 || control_endpoint == 0 {
        return Err(Error::InvalidArgument);
    }

    let payload = encode_names(service_name, endpoint_name);
    let msg = make_payload_message(REGISTRY_SUBSCRIBE_LABEL, payload.len(), &[control_endpoint]);
    // Fire-and-forget request; caller will handle the grant later.
    send_with_payload(registry_endpoint, &msg, &payload)?;
    Ok(())
}

/// Drain any pending grant traffic without blocking.
pub fn handle_grant_requests() -> Result<()> {
    let control_endpoint = control_endpoint();
    if control_endpoint == 0 {
        return Err(Error::InvalidArgument);
    }

    let mut buf = [0u8; 256];
    loop {
        match ipc_recv_nonblocking(control_endpoint, &mut buf) {
            Ok(len) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    let _ = handle_incoming_message(&msg, payload)?;
                }
            }
            Err(Error::WouldBlock) => return Ok(()),
            Err(err) => return Err(err),
        }
    }
}

pub fn handle_incoming_message(msg: &Message, payload: &[u8]) -> Result<Option<RegistryEvent>> {
    match msg.tag.label {
        REGISTRY_GRANT_REQUEST_LABEL => {
            handle_grant_request(msg, payload)?;
            Ok(None)
        }
        REGISTRY_GRANT_DELIVER_LABEL => {
            let (service_name, name) = decode_names(payload).ok_or(Error::InvalidArgument)?;
            Ok(Some(RegistryEvent::Grant {
                service_name,
                name,
                token: msg.words[1],
            }))
        }
        REGISTRY_SUBSCRIBE_REPLY_LABEL => {
            let code = msg.words[1] as i32;
            Ok(Some(RegistryEvent::SubscribeStatus { code }))
        }
        _ => Ok(None),
    }
}

pub fn poll_or_recv_any(
    endpoints: &[usize],
    buf: &mut [u8],
    timeout_ms: u64,
) -> Result<(usize, usize)> {
    let control_endpoint = control_endpoint();
    let mut tokens = Vec::with_capacity(endpoints.len() + 1);
    tokens.extend_from_slice(endpoints);
    // Always include the registry control endpoint if configured.
    if control_endpoint != 0 {
        tokens.push(control_endpoint);
    }
    ipc_recv_any(&tokens, buf, timeout_ms)
}

fn wait_for_grant(service_name: &str, endpoint_name: &str) -> Result<usize> {
    let control_endpoint = control_endpoint();
    let mut buf = [0u8; 256];
    // Block forever. If the producer dies before granting, the kernel
    // revokes its tokens and our recv returns a concrete error
    // (BadToken / Closed). A time-bounded recv would convert "hung" into
    // "spurious NotFound" and trigger cascade kills — banned by the no-
    // timeouts rule (memory/feedback_no_timeouts.md). A hang is data.
    loop {
        let (index, len) = ipc_recv_any(&[control_endpoint], &mut buf, u64::MAX)?;
        let _ = index;
        if let Some((msg, payload)) = parse_message(&buf[..len]) {
            if let Some(event) = handle_incoming_message(&msg, payload)? {
                match event {
                    RegistryEvent::Grant { service_name: svc, name, token } => {
                        if name == endpoint_name && svc == service_name {
                            return Ok(token);
                        }
                    }
                    RegistryEvent::SubscribeStatus { code } => {
                        if code != 0 {
                            let _ = crate::debug_print(&alloc::format!(
                                "registry-client: subscribe FAIL status={} for {}:{}",
                                code,
                                service_name,
                                endpoint_name
                            ));
                            return Err(error_from_code(code));
                        }
                    }
                }
            }
        }
    }
}

fn handle_grant_request(msg: &Message, payload: &[u8]) -> Result<()> {
    let requester_endpoint = msg.words[1];
    if requester_endpoint == 0 {
        return Err(Error::InvalidArgument);
    }
    // Payload encodes the output name being requested.
    let endpoint_name = decode_single_name(payload).ok_or(Error::InvalidArgument)?;
    // Look up the local output token and service name for the grant reply.
    let (endpoint_token, service_name) = {
        let state = REGISTRY_STATE.lock();
        (
            state.outputs.get(&endpoint_name).copied().unwrap_or(0),
            state.service_name.clone().unwrap_or_default(),
        )
    };
    if endpoint_token == 0 {
        let _ = crate::debug_print(&alloc::format!(
            "registry-client: missing output {}",
            endpoint_name
        ));
        return Ok(());
    }
    // Mint a least-privilege token that only allows IPC_SEND.
    let send_rights = (Rights::IPC_SEND | Rights::IPC_CALL).bits() as usize;
    let _ = crate::debug_print(&alloc::format!(
        "registry-client: handle_grant_request endpoint={} requester={}",
        endpoint_name, requester_endpoint
    ));
    let derived = token_derive(endpoint_token, send_rights, u64::MAX)?;
    let payload = encode_names(&service_name, &endpoint_name);
    let reply = make_payload_message(REGISTRY_GRANT_DELIVER_LABEL, payload.len(), &[derived]);
    match send_with_payload_nonblocking(requester_endpoint, &reply, &payload) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = crate::debug_print(&alloc::format!(
                "registry-client: grant deliver failed {:?}",
                err
            ));
            Err(err)
        }
    }
}

use crate::ipc::{call_with_payload, make_payload_message, parse_message};

fn send_with_payload(endpoint: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    crate::syscall::ipc_send(endpoint, &buffer)
}

fn send_with_payload_nonblocking(endpoint: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    match crate::syscall::ipc_send_nonblocking(endpoint, &buffer) {
        Ok(()) => Ok(()),
        Err(crate::error::Error::WouldBlock) => Ok(()),
        Err(err) => Err(err),
    }
}

/// Synchronous (call-style) send with payload: blocks until the registry replies.
/// Returns Ok(()) if the registry accepted, or the error it reported.
fn call_and_check(endpoint: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let mut reply = Message::new(0, [0; 6], 0);
    call_with_payload(endpoint, msg, payload, &mut reply)?;
    let code = reply.words[1] as i32;
    if code == 0 {
        Ok(())
    } else {
        Err(error_from_code(code))
    }
}

fn encode_names(service_name: &str, endpoint_name: &str) -> Vec<u8> {
    let service_bytes = service_name.as_bytes();
    let endpoint_bytes = endpoint_name.as_bytes();
    let mut payload = Vec::with_capacity(4 + service_bytes.len() + endpoint_bytes.len());
    let service_len = service_bytes.len() as u16;
    let endpoint_len = endpoint_bytes.len() as u16;
    payload.extend_from_slice(&service_len.to_le_bytes());
    payload.extend_from_slice(&endpoint_len.to_le_bytes());
    payload.extend_from_slice(service_bytes);
    payload.extend_from_slice(endpoint_bytes);
    payload
}

fn decode_single_name(payload: &[u8]) -> Option<String> {
    let (service, _endpoint) = decode_names(payload)?;
    Some(service)
}

fn decode_names(payload: &[u8]) -> Option<(String, String)> {
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

fn error_from_code(code: i32) -> Error {
    if code == 0 {
        return Error::InvalidOperation;
    }
    Error::from_errno(code as isize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_names_roundtrip() {
        let payload = encode_names("tty", "write");
        let (service, endpoint) = decode_names(&payload).expect("decode");
        assert_eq!(service, "tty");
        assert_eq!(endpoint, "write");
    }
}
