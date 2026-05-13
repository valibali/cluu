#![no_std]
#![no_main]

//! Virtual Terminal Manager for CLUU.
//!
//! vtmgr is a pure IPC coordinator that owns VT lifecycle. It receives
//! switch requests from kbd (VTMGR_SWITCH_VT_LABEL), asks console to
//! create VT buffers (CONSOLE_CREATE_VT_LABEL), asks procmgr to spawn
//! tty:N (via PROCMGR_SPAWN_SERVICE_LABEL), and sends CONSOLE_ACTIVATE/DEACTIVATE
//! to switch the visible VT.

extern crate alloc;

mod context;
mod input_routing;

use context::VtmgrContext;
use libcluu::ipc::{parse_message, KBD_EVENT_LABEL, VTMGR_PIN_VT_LABEL, VTMGR_SWITCH_VT_LABEL};
use libcluu::types::Message;
use libcluu::{debug_print, yield_cpu, Result};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    let mut ctx = VtmgrContext::new()?;
    let mut buf = [0u8; 128];
    let mut saw_error = false;

    loop {
        ctx.ensure_subscriptions();

        let tokens = [ctx.endpoint, ctx.registry_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                saw_error = false;
                let Some((msg, payload)) = parse_message(&buf[..len]) else {
                    continue;
                };

                if index == 1 {
                    ctx.handle_registry_message(&msg, payload);
                    continue;
                }

                handle_vtmgr_message(&mut ctx, &msg, payload);
            }
            Err(err) => {
                if err != libcluu::Error::WouldBlock && !saw_error {
                    saw_error = true;
                    let _ = debug_print("vtmgr: recv error");
                }
                let _ = yield_cpu();
            }
        }
    }
}

fn handle_vtmgr_message(ctx: &mut VtmgrContext, msg: &Message, payload: &[u8]) {
    match msg.tag.label {
        KBD_EVENT_LABEL => {
            ctx.router.forward(&msg, |kind| ctx.lookup_target_endpoint(kind));
        }
        VTMGR_SWITCH_VT_LABEL if msg.tag.words >= 1 => {
            let target_vt = msg.words[0];
            ctx.switch_vt(target_vt);
        }
        VTMGR_PIN_VT_LABEL if msg.tag.words >= 1 => {
            let vt_index = msg.words[0];
            // Payload is the service name as raw UTF-8 bytes (no NUL terminator).
            if let Ok(name) = core::str::from_utf8(payload) {
                ctx.handle_pin_vt(vt_index, name);
            }
        }
        _ => {}
    }
}

