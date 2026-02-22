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

use context::VtmgrContext;
use libcluu::ipc::VTMGR_SWITCH_VT_LABEL;
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

                handle_vtmgr_message(&mut ctx, &msg);
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

fn handle_vtmgr_message(ctx: &mut VtmgrContext, msg: &Message) {
    if msg.tag.label == VTMGR_SWITCH_VT_LABEL && msg.tag.words >= 1 {
        let target_vt = msg.words[0];
        ctx.switch_vt(target_vt);
    }
}

fn parse_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    if buf.len() < core::mem::size_of::<Message>() {
        return None;
    }
    let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
    let header = core::mem::size_of::<Message>();
    let mut payload_len = msg.words[0];
    if header + payload_len > buf.len() {
        payload_len = 0;
    }
    Some((msg, &buf[header..header + payload_len]))
}
