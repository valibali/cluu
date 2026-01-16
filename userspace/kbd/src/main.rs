//! Minimal keyboard service.

#![no_std]
#![no_main]

use core::mem::size_of;
use libcluu::boot::process_info;
use libcluu::ipc::{send, KBD_EVENT_LABEL};
use libcluu::registry;
use libcluu::types::Message;
use libcluu::{debug_print, irq_attach, yield_cpu, Error, Result};

// Token indices (set by init)
const SVC_TOKEN_LISTEN: usize = 6;
const SVC_TOKEN_IRQ: usize = 8;

const KEYBOARD_IRQ: usize = 1;

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
    let irq_token = info.tokens[SVC_TOKEN_IRQ];
    registry::init("kbd")?;
    registry::register_default_outputs()?;
    let registry_endpoint = registry::control_endpoint();

    debug_print("kbd: ready")?;
    irq_attach(irq_token, endpoint, KEYBOARD_IRQ)?;
    debug_print("kbd: irq attached")?;
    yield_cpu()?;

    let mut buf = [0u8; 64];
    let mut saw_error = false;
    let mut tty_endpoint = 0usize;
    let mut requested_tty = false;
    loop {
        if tty_endpoint == 0
            && !requested_tty
            && registry::request_subscription("tty", "main").is_ok()
        {
            requested_tty = true;
        }

        let tokens = [endpoint, registry_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                let Some((msg, payload)) = parse_message(&buf[..len]) else {
                    continue;
                };
                if index == 1 {
                    if let Ok(Some(event)) = registry::handle_incoming_message(&msg, payload) {
                        match event {
                            registry::RegistryEvent::Grant { name, token } => {
                                if name == "main" {
                                    tty_endpoint = token;
                                    let _ = debug_print("kbd: tty subscribed");
                                }
                            }
                            registry::RegistryEvent::SubscribeStatus { code } => {
                                if code != 0 {
                                    requested_tty = false;
                                }
                            }
                        }
                    }
                    continue;
                }

                if msg.tag.label == KBD_EVENT_LABEL && msg.tag.words >= 1 {
                    let scancode = msg.words[0] as u8;
                    if let Some(ascii) = scancode_to_ascii(scancode) {
                        let msg = Message::new(KBD_EVENT_LABEL, [0, ascii as usize, 0, 0, 0, 0], 2);
                        if tty_endpoint != 0 {
                            let _ = send(tty_endpoint, &msg, libcluu::types::IpcFlags::empty());
                        }
                    }
                }
            }
            Err(Error::WouldBlock) => {
                let _ = yield_cpu();
            }
            Err(_) => {
                if !saw_error {
                    saw_error = true;
                    let _ = debug_print("kbd: recv error");
                }
                let _ = yield_cpu();
            }
        }
    }
}

fn parse_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    if buf.len() < size_of::<Message>() {
        return None;
    }
    let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
    let mut payload_len = msg.words[0];
    let header = size_of::<Message>();
    if header + payload_len > buf.len() {
        payload_len = 0;
    }
    let end = header + payload_len;
    Some((msg, &buf[header..end]))
}

fn scancode_to_ascii(scancode: u8) -> Option<u8> {
    if scancode & 0x80 != 0 {
        return None;
    }
    match scancode {
        0x02 => Some(b'1'),
        0x03 => Some(b'2'),
        0x04 => Some(b'3'),
        0x05 => Some(b'4'),
        0x06 => Some(b'5'),
        0x07 => Some(b'6'),
        0x08 => Some(b'7'),
        0x09 => Some(b'8'),
        0x0A => Some(b'9'),
        0x0B => Some(b'0'),
        0x10 => Some(b'q'),
        0x11 => Some(b'w'),
        0x12 => Some(b'e'),
        0x13 => Some(b'r'),
        0x14 => Some(b't'),
        0x15 => Some(b'y'),
        0x16 => Some(b'u'),
        0x17 => Some(b'i'),
        0x18 => Some(b'o'),
        0x19 => Some(b'p'),
        0x1E => Some(b'a'),
        0x1F => Some(b's'),
        0x20 => Some(b'd'),
        0x21 => Some(b'f'),
        0x22 => Some(b'g'),
        0x23 => Some(b'h'),
        0x24 => Some(b'j'),
        0x25 => Some(b'k'),
        0x26 => Some(b'l'),
        0x2C => Some(b'z'),
        0x2D => Some(b'x'),
        0x2E => Some(b'c'),
        0x2F => Some(b'v'),
        0x30 => Some(b'b'),
        0x31 => Some(b'n'),
        0x32 => Some(b'm'),
        0x39 => Some(b' '),
        0x1C => Some(b'\n'),
        0x0E => Some(0x08),
        _ => None,
    }
}
