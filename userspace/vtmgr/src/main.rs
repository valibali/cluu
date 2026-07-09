#![no_std]
#![no_main]

//! Virtual Terminal Manager for CLUU.
//!
//! vtmgr is a pure IPC coordinator that owns VT lifecycle. It receives
//! switch requests from kbd (VTMGR_REQUEST_VT_SWITCH_LABEL), asks console to
//! create VT buffers (CONSOLE_CREATE_VT_LABEL), asks procmgr to spawn
//! tty:N (via PROCMGR_SPAWN_SERVICE_LABEL), and sends CONSOLE_ACTIVATE/DEACTIVATE
//! to switch the visible VT.
//!
//! First server adopted to `AsyncServerMain` (wire-validation PoC). vtmgr
//! itself has no downstream blocking `ipc::call` — all downstream is
//! fire-and-forget `send` — so the async runtime is exercised but not
//! load-bearing here. The PoC proves the skeleton drops into a real server
//! loop and the build holds.

extern crate alloc;

mod context;
mod input_routing;

use context::VtmgrContext;
use libcluu::boot::{process_info, TOKEN_SELF};
use libcluu::ipc::{
    parse_message, KBD_EVENT_LABEL, MOUSE_EVENT_LABEL, VTMGR_PIN_VT_LABEL, VTMGR_REQUEST_VT_SWITCH_LABEL,
};
use libcluu::server_main::AsyncServerMain;
use libcluu::types::{IpcFlags, Message};
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

    let token_self = process_info().tokens[TOKEN_SELF];
    let mut server = AsyncServerMain::new(token_self, ctx.endpoint)?;

    loop {
        ctx.ensure_subscriptions();
        server.poll_ready();

        let tokens = [ctx.endpoint, ctx.registry_endpoint, server.reply_endpoint()];
        match server.recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((msg, payload, _len, index)) => {
                saw_error = false;

                if index == 1 {
                    ctx.handle_registry_message(&msg, &payload);
                    continue;
                }

                // index 2 = reply to our own async call (none today, but drain it)
                if index == 2 {
                    if let Some(cookie) = libcluu::ipc::extract_reply_id(&msg) {
                        server.deliver_reply(cookie, msg, payload);
                    }
                    continue;
                }

                handle_vtmgr_message(&mut ctx, &msg, &payload);
            }
            Err(err) => {
                if err != libcluu::Error::WouldBlock && !saw_error {
                    saw_error = true;
                    let _ = debug_print("vtmgr: recv error");
                }
                let _ = yield_cpu();
            }
        }
        server.drain_completions();
    }
}

fn handle_vtmgr_message(ctx: &mut VtmgrContext, msg: &Message, payload: &[u8]) {
    match msg.tag.label {
        KBD_EVENT_LABEL | MOUSE_EVENT_LABEL => {
            ctx.router.forward(&msg, |kind| ctx.lookup_target_endpoint(kind));
        }
        VTMGR_REQUEST_VT_SWITCH_LABEL => {
            let new_vt = msg.words[0];
            let allowed = ctx.router.should_allow_switch(
                ctx.active_vt as u8, new_vt as u8
            );
            let err: u64 = if !allowed {
                16 // EBUSY
            } else if new_vt >= context::VT_COUNT {
                22 // EINVAL
            } else {
                ctx.switch_vt(new_vt);
                0
            };
            if let Some(reply_id) = libcluu::ipc::extract_reply_id(&msg) {
                let reply = Message::new(0, [err as usize, 0, 0, 0, 0, 0], 1);
                let _ = libcluu::ipc::reply(reply_id, &reply, IpcFlags::empty());
            }
        }
        VTMGR_PIN_VT_LABEL if msg.tag.words >= 2 => {
            // words[0] = payload_len (overwritten by send_msg_with_payload),
            // words[1] = vt_index. Payload = service name as raw UTF-8.
            let vt_index = msg.words[1];
            if let Ok(name) = core::str::from_utf8(payload) {
                ctx.handle_pin_vt(vt_index, name);
            }
        }
        _ => {}
    }
}

