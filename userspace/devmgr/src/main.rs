#![no_std]
#![no_main]

//! devmgr — CLUU device manager.
//!
//! General device registry and capability broker for all device classes
//! (block, char, input, framebuffer). Drivers register at boot; procmgr
//! queries at spawn time to build VFS views; VFS queries for `/dev`
//! enumeration.
//!
//! Architecture (SOLID):
//! - `device.rs`   — domain types (DeviceClass, DeviceEntry). No deps.
//! - `registry.rs` — DevRegistry: pure data model + queries. No IPC.
//! - `handlers.rs` — one fn per IPC label. Controllers only.
//! - `main.rs`     — orchestration: init, recv, dispatch. No business logic.
//!
//! Sync recv loop: devmgr is a leaf service — handlers do local syscalls
//! (token derive/revoke) and state updates, no downstream IPC. The async
//! runtime is not needed here; it lives on the VFS side (Phase 2) where
//! `DevRegistryBackend` calls devmgr via `IpcCallFuture`. All handlers use
//! `reply_to_sender*` so async callers don't hit the silent-reply-drop
//! gotcha (see KB: cluu-pts-verb-async-reply-tag-silent-drop).

extern crate alloc;

mod device;
mod dev_registry;
mod handlers;

use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2};
use libcluu::ipc::{
    parse_message, DEVMGR_GRANT_DEVICE_LABEL, DEVMGR_GRANT_REGION_LABEL,
    DEVMGR_LIST_FOR_ENVELOPE_LABEL, DEVMGR_MINT_IRQ_CAP_LABEL, DEVMGR_REGISTER_CHAR_LABEL,
    DEVMGR_REGISTER_LABEL, DEVMGR_REVOKE_LABEL,
};
use libcluu::registry;
use libcluu::syscall::ipc_recv_any_with_sender;
use libcluu::{debug_print, yield_cpu, Error, Result};

use dev_registry::DevRegistry;

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
    let boot_root_block_token = info.tokens[TOKEN_EXTRA_1];
    let boot_root_device_token = info.tokens[TOKEN_EXTRA_1];
    let irq_handle_root_token = info.tokens[TOKEN_EXTRA_2];

    registry::init("devmgr")?;
    registry::register_output("main", endpoint)?;
    let _ = debug_print("devmgr: ready");

    let control_endpoint = registry::control_endpoint();
    let mut registry = DevRegistry::new();
    let mut buf = [0u8; 512];

    loop {
        let tokens = [endpoint, control_endpoint];
        match ipc_recv_any_with_sender(&tokens, &mut buf, u64::MAX) {
            Ok((idx, len, _sender_tid)) => {
                if len < core::mem::size_of::<libcluu::types::Message>() {
                    continue;
                }
                if idx == 1 {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let _ = registry::handle_incoming_message(&msg, payload);
                    }
                    continue;
                }
                let msg = unsafe {
                    &*(buf.as_ptr() as *const libcluu::types::Message)
                };
                let payload = &buf[core::mem::size_of::<libcluu::types::Message>()..len];
                match msg.tag.label {
                    DEVMGR_REGISTER_LABEL => {
                        handlers::handle_register_block(
                            &mut registry,
                            msg,
                            endpoint,
                            boot_root_block_token,
                        );
                    }
                    DEVMGR_REGISTER_CHAR_LABEL => {
                        handlers::handle_register_char(
                            &mut registry,
                            msg,
                            endpoint,
                            boot_root_device_token,
                        );
                    }
                    DEVMGR_GRANT_REGION_LABEL => {
                        handlers::handle_grant_region(&registry, msg, endpoint);
                    }
                    DEVMGR_GRANT_DEVICE_LABEL => {
                        handlers::handle_grant_device(&registry, msg, endpoint);
                    }
                    DEVMGR_REVOKE_LABEL => {
                        handlers::handle_revoke(msg, endpoint);
                    }
                    DEVMGR_MINT_IRQ_CAP_LABEL => {
                        handlers::handle_mint_irq_cap(msg, endpoint, irq_handle_root_token);
                    }
                    DEVMGR_LIST_FOR_ENVELOPE_LABEL => {
                        handlers::handle_list_for_envelope(
                            &registry,
                            msg,
                            payload,
                            endpoint,
                        );
                    }
                    _ => {}
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
