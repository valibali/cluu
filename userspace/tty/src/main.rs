//! Minimal TTY service (echo + stdin forwarding).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, vec::Vec};
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};
use libcluu::boot::process_info;
use libcluu::ipc::{
    send_with_payload, CONSOLE_WRITE_LABEL, KBD_EVENT_LABEL, TTY_READ_LABEL, TTY_REGISTER_LABEL,
    TTY_WRITE_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, ipc_recv, yield_cpu, Error, Result};

// Token indices (set by init)
const SVC_TOKEN_LISTEN: usize = 0;
const SVC_TOKEN_CONSOLE_SEND: usize = 2;

static LOG_WRITE_SEEN: AtomicBool = AtomicBool::new(false);
static LOG_REGISTER_SEEN: AtomicBool = AtomicBool::new(false);

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
    let console_endpoint = info.tokens[SVC_TOKEN_CONSOLE_SEND];

    debug_print(&format!(
        "tty: endpoint {} console {}",
        endpoint, console_endpoint
    ))?;
    debug_print("tty: ready")?;
    yield_cpu()?;

    let mut shell_stdin: usize = 0;
    let mut line: Vec<u8> = Vec::new();
    let mut saw_key = false;

    let mut buf = [0u8; 256];
    loop {
        match ipc_recv(endpoint, &mut buf) {
            Ok(len) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    match msg.tag.label {
                        KBD_EVENT_LABEL => {
                            let ch = msg.words[1] as u8;
                            if !saw_key {
                                saw_key = true;
                                let _ = debug_print("tty: first key event");
                            }
                            handle_char(console_endpoint, shell_stdin, ch, &mut line)?;
                        }
                        TTY_WRITE_LABEL => {
                            if !LOG_WRITE_SEEN.swap(true, Ordering::Relaxed) {
                                let _ = debug_print("tty: forward to console");
                            }
                            let _ =
                                send_with_payload(console_endpoint, CONSOLE_WRITE_LABEL, payload);
                        }
                        TTY_REGISTER_LABEL => {
                            shell_stdin = msg.words[0];
                            if !LOG_REGISTER_SEEN.swap(true, Ordering::Relaxed) {
                                let _ = debug_print("tty: stdin registered");
                            }
                        }
                        _ => {}
                    }
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

fn parse_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    if buf.len() < size_of::<Message>() {
        return None;
    }
    let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
    let payload_len = msg.words[0];
    let header = size_of::<Message>();
    let end = header + payload_len;
    if end > buf.len() {
        return None;
    }
    Some((msg, &buf[header..end]))
}

fn handle_char(
    console_endpoint: usize,
    shell_stdin: usize,
    ch: u8,
    line: &mut Vec<u8>,
) -> Result<()> {
    match ch {
        b'\n' => {
            line.push(ch);
            let _ = send_with_payload(console_endpoint, CONSOLE_WRITE_LABEL, b"\n");
            forward_to_shell(shell_stdin, ch);
            line.clear();
        }
        0x08 => {
            if !line.is_empty() {
                line.pop();
                let _ = send_with_payload(console_endpoint, CONSOLE_WRITE_LABEL, b"\x08 \x08");
                forward_to_shell(shell_stdin, ch);
            }
        }
        _ => {
            line.push(ch);
            let _ = send_with_payload(console_endpoint, CONSOLE_WRITE_LABEL, &[ch]);
            forward_to_shell(shell_stdin, ch);
        }
    }
    Ok(())
}

fn forward_to_shell(shell_stdin: usize, ch: u8) {
    if shell_stdin == 0 {
        return;
    }
    let msg = Message::new(TTY_READ_LABEL, [ch as usize, 0, 0, 0, 0, 0], 1);
    let _ = libcluu::ipc::send(shell_stdin, &msg, IpcFlags::empty());
}
