//! Minimal keyboard service.

#![no_std]
#![no_main]

use core::mem::size_of;
use libcluu::ipc::{send, KBD_EVENT_LABEL};
use libcluu::types::Message;
use libcluu::{debug_print, ipc_recv, irq_attach, kbd_info, yield_cpu, Error, Result};

const KEYBOARD_IRQ: usize = 1;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    let info = kbd_info();
    debug_print("kbd: ready")?;
    irq_attach(info.irq_token, info.endpoint, KEYBOARD_IRQ)?;
    debug_print("kbd: irq attached")?;
    yield_cpu()?;

    let mut buf = [0u8; 64];
    let mut saw_key = false;
    let mut saw_msg = false;
    let mut saw_error = false;
    loop {
        match ipc_recv(info.endpoint, &mut buf) {
            Ok(len) => {
                if !saw_msg {
                    saw_msg = true;
                    let _ = debug_print("kbd: received message");
                }
                if let Some(msg) = parse_message(&buf[..len]) {
                    if msg.tag.label == KBD_EVENT_LABEL && msg.tag.words >= 1 {
                        let scancode = msg.words[0] as u8;
                        if !saw_key {
                            saw_key = true;
                            let _ = debug_print("kbd: first key event");
                        }
                        if let Some(ascii) = scancode_to_ascii(scancode) {
                            // words[0] = payload len (0), words[1] = ascii char
                            let msg =
                                Message::new(KBD_EVENT_LABEL, [0, ascii as usize, 0, 0, 0, 0], 2);
                            let _ =
                                send(info.tty_endpoint, &msg, libcluu::types::IpcFlags::empty());
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

fn parse_message(buf: &[u8]) -> Option<Message> {
    if buf.len() < size_of::<Message>() {
        return None;
    }
    Some(unsafe { (buf.as_ptr() as *const Message).read_unaligned() })
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
