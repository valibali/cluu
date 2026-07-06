#![no_std]
#![no_main]

//! inputd — CLUU input daemon.
//!
//! Thin intermediary between kbd/mouse and vtmgr. Receives decoded input
//! events, buffers them for /dev/input/* reads, and forwards to vtmgr:input
//! for VT-aware routing.
//!
//! Flow:
//!   kbd  ──KBD_EVENT_LABEL──►  inputd  ──KBD_EVENT_LABEL──►  vtmgr ──► compositor/tty
//!   mouse ──MOUSE_EVENT_LABEL──► inputd  ──MOUSE_EVENT_LABEL──► vtmgr ──► compositor/tty
//!
//! /dev/input/* slow path:
//!   cat /dev/input/kbd ──► VFS ──DEV_READ_REQUEST_LABEL──► inputd ──► buffered InputEvent bytes

extern crate alloc;

mod context;

use context::InputdContext;
use libcluu::input::InputEvent;
use libcluu::ipc::{
    parse_message, KBD_EVENT_LABEL, MOUSE_EVENT_LABEL,
};
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
    let mut ctx = InputdContext::new()?;
    let mut buf = [0u8; 256];
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

                handle_inputd_message(&mut ctx, &msg);
            }
            Err(err) => {
                if err != libcluu::Error::WouldBlock && !saw_error {
                    saw_error = true;
                    let _ = debug_print("inputd: recv error");
                }
                let _ = yield_cpu();
            }
        }
    }
}

fn handle_inputd_message(ctx: &mut InputdContext, msg: &Message) {
    match msg.tag.label {
        KBD_EVENT_LABEL => {
            let event = InputEvent::Key {
                ascii: msg.words[1] as u8,
                scancode: msg.words[3] as u8,
                modifiers: msg.words[2] as u8,
                extended: msg.words[4] as u8,
            };
            ctx.buffer_kbd(event.encode());
            ctx.forward_to_vtmgr(msg);
        }
        MOUSE_EVENT_LABEL => {
            let event = InputEvent::Mouse {
                dx: msg.words[1] as i32,
                dy: msg.words[2] as i32,
                buttons: msg.words[3] as u8,
            };
            ctx.buffer_mouse(event.encode());
            ctx.forward_to_vtmgr(msg);
        }
        libcluu::ipc::DEV_READ_REQUEST_LABEL => {
            ctx.handle_read_request(msg);
        }
        _ => {}
    }
}
