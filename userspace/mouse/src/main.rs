#![no_std]
#![no_main]

//! PS/2 mouse service for CLUU.
//!
//! Receives raw IRQ12 scancodes from the kernel IRQ bridge, reassembles
//! 3-byte PS/2 mouse packets, and forwards decoded mouse events to
//! vtmgr:input for routing to the compositor.

extern crate alloc;

mod context;
mod packet;
mod protocol;

use context::MouseContext;
use libcluu::ipc::KBD_RAW_LABEL;
use libcluu::types::Message;
use libcluu::Result;
use packet::PacketParser;
use protocol::build_mouse_event;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    let mut ctx = MouseContext::new()?;
    let mut parser = PacketParser::new();
    let mut buf = [0u8; 128];

    loop {
        ctx.ensure_subscriptions();

        let tokens = [ctx.endpoint, ctx.registry_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                let Some((msg, payload)) = protocol::parse_message(&buf[..len]) else {
                    continue;
                };

                if index == 1 {
                    ctx.handle_registry_message(&msg, payload);
                    continue;
                }

                handle_mouse_byte(&mut ctx, &mut parser, &msg);
            }
            Err(_) => {
                let _ = libcluu::yield_cpu();
            }
        }
    }
}

fn handle_mouse_byte(ctx: &mut MouseContext, parser: &mut PacketParser, msg: &Message) {
    if msg.tag.label != KBD_RAW_LABEL || msg.tag.words < 1 {
        return;
    }
    let byte = msg.words[0] as u8;

    if let Some(event) = parser.feed(byte) {
        let msg = build_mouse_event(event.dx, event.dy, event.buttons);
        ctx.send_to_router(&msg);
    }
}
