#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use core::mem::size_of;
use libcluu::boot::{process_info, TOKEN_STDIN, TOKEN_STDOUT, TOKEN_STDERR, TOKEN_STDLOG};
use libcluu::ipc::{send_with_payload, TTY_READ_LABEL, TTY_WRITE_LABEL};
use libcluu::types::Message;
use libcluu::{debug_print, ipc_recv, yield_cpu, Error, Result};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    let info = process_info();
    let stdin = info.tokens[TOKEN_STDIN];
    let stdout = info.tokens[TOKEN_STDOUT];
    let stderr = info.tokens[TOKEN_STDERR];
    let stdlog = info.tokens[TOKEN_STDLOG];

    debug_print("shell: ready")?;
    let _ = debug_print(&format!(
        "shell: stdin {} stdout {} stderr {} stdlog {}",
        stdin, stdout, stderr, stdlog
    ));
    if let Err(_) = print_prompt(stdout) {
        let _ = debug_print("shell: prompt write failed");
    }

    let mut buf = [0u8; 128];
    loop {
        match ipc_recv(stdin, &mut buf) {
            Ok(len) => {
                if let Some((msg, _payload)) = parse_message(&buf[..len]) {
                    if msg.tag.label == TTY_READ_LABEL && msg.tag.words >= 1 {
                        let ch = msg.words[0] as u8;
                        if ch == b'\n' {
                            print_prompt(stdout)?;
                        }
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

fn print_prompt(endpoint: usize) -> Result<()> {
    send_with_payload(endpoint, TTY_WRITE_LABEL, b"cluu> ")?;
    Ok(())
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
