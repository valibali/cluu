//! regdeny probe: verifies that unregister from a cross-session service is rejected.
//! Lifted from RegistryDenyBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_REGISTRY, TOKEN_STDOUT};
use libcluu::{registry, syscall};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let service = args.get(1).map_or("tty:0", |s| s.as_str());
    let endpoint = args.get(2).map_or("main", |s| s.as_str());

    let registry_endpoint = process_info().tokens[TOKEN_REGISTRY];
    if registry_endpoint == 0 {
        let _ = libcluu::debug_print("regdeny: FAIL missing registry token");
        return 1;
    }

    let payload = encode_registry_names(service, endpoint);
    let mut req = libcluu::types::Message::new(registry::REGISTRY_UNREGISTER_LABEL, [0; 6], 2);
    req.words[0] = payload.len();
    req.words[1] = process_info().tokens[TOKEN_STDOUT];
    let header = req.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(&payload);

    match syscall::ipc_send(registry_endpoint, &buffer) {
        Ok(()) => {
            let line = format!(
                "regdeny: PASS permission denied service={} endpoint={}",
                service, endpoint
            );
            let _ = libcluu::debug_print(&line);
            0
        }
        Err(err) => {
            let line = format!(
                "regdeny: FAIL send error {:?} service={} endpoint={}",
                err, service, endpoint
            );
            let _ = libcluu::debug_print(&line);
            1
        }
    }
}

fn encode_registry_names(service: &str, endpoint: &str) -> Vec<u8> {
    let service_bytes = service.as_bytes();
    let endpoint_bytes = endpoint.as_bytes();
    let mut payload = Vec::with_capacity(4 + service_bytes.len() + endpoint_bytes.len());
    payload.extend_from_slice(&(service_bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(endpoint_bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(service_bytes);
    payload.extend_from_slice(endpoint_bytes);
    payload
}
