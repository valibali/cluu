#![no_std]
#![no_main]

extern crate alloc;

mod state;
mod shm;
mod protocol;

use alloc::format;
use libcluu::boot::{process_info, TOKEN_IPC};
use libcluu::{debug_print, registry, syscall, Error};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("compositor: init");
    let mut comp = match state::Compositor::init() {
        Ok(c) => c,
        Err(_) => {
            let _ = debug_print("compositor: init failed");
            return -1;
        }
    };

    let info = process_info();
    let ipc_cap = info.tokens[TOKEN_IPC];
    comp.instance_id = 0;
    let service_name = format!("compositor:{}", comp.instance_id);
    if registry::init(&service_name).is_err() {
        let _ = debug_print("compositor: registry init failed");
        return -1;
    }
    if registry::register_default_outputs().is_err() {
        let _ = debug_print("compositor: register_default_outputs failed");
    }

    comp.client_endpoint = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => { let _ = debug_print("compositor: client endpoint failed"); return -1; }
    };
    comp.input_endpoint_global = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => { let _ = debug_print("compositor: input endpoint failed"); return -1; }
    };
    comp.control_endpoint = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => { let _ = debug_print("compositor: control endpoint failed"); return -1; }
    };
    let _ = registry::register_output("client", comp.client_endpoint);
    let _ = registry::register_output("input", comp.input_endpoint_global);
    let _ = registry::register_output("control", comp.control_endpoint);
    comp.registry_endpoint = registry::control_endpoint();

    let _ = debug_print("compositor: endpoints registered");
    let _ = debug_print("compositor: ready");

    let tokens = [
        comp.client_endpoint,
        comp.input_endpoint_global,
        comp.control_endpoint,
        comp.registry_endpoint,
    ];
    let mut buf = [0u8; 1024];

    loop {
        match syscall::ipc_recv_any(&tokens, &mut buf, 1000) {
            Ok((idx, len)) => {
                if let Some((msg, payload)) = libcluu::ipc::parse_message(&buf[..len]) {
                    let kind = protocol::parse(&msg);
                    let _ = debug_print("compositor: msg");
                    let _ = (idx, payload, kind);
                }
            }
            Err(Error::Timeout) | Err(Error::WouldBlock) => {}
            Err(_) => { let _ = syscall::yield_cpu(); }
        }
    }
}
