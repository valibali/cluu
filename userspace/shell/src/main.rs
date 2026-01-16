#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use core::mem::size_of;
use libcluu::boot::{process_info, TOKEN_STDERR, TOKEN_STDIN, TOKEN_STDLOG};
use libcluu::ipc::{send_with_payload, TTY_READ_LABEL, TTY_WRITE_LABEL};
use libcluu::registry;
use libcluu::types::Message;
use libcluu::{debug_print, yield_cpu, Error, Result};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    let info = process_info();
    registry::init("shell")?;
    registry::register_default_outputs()?;
    let stdin = info.tokens[TOKEN_STDIN];
    let stderr = info.tokens[TOKEN_STDERR];
    let stdlog = info.tokens[TOKEN_STDLOG];
    let stdout = loop {
        // Lazily subscribe to tty's main output and use it as stdout.
        match registry::subscribe_output("tty", "main") {
            Ok(token) => break token,
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    };
    let registry_endpoint = registry::control_endpoint();

    debug_print("shell: ready")?;
    let _ = debug_print(&format!(
        "shell: stdin {} stdout {} stderr {} stdlog {}",
        stdin, stdout, stderr, stdlog
    ));
    let _ = print_prompt(stdout);

    let mut buf = [0u8; 128];
    loop {
        // Wait for either keyboard input via stdin or registry control traffic.
        let tokens = [stdin, registry_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    if index == 1 {
                        // Registry control messages (grants/status).
                        let _ = registry::handle_incoming_message(&msg, payload);
                        continue;
                    }
                    if msg.tag.label == TTY_READ_LABEL {
                        if !payload.is_empty() {
                            handle_line_payload(stdout, payload)?;
                        } else if msg.tag.words >= 2 {
                            let ch = msg.words[1] as u8;
                            handle_line_payload(stdout, &[ch])?;
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
    // Prompt is sent to tty, which forwards to console.
    send_with_payload(endpoint, TTY_WRITE_LABEL, b"cluu> ")?;
    Ok(())
}

fn parse_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    // Message header followed by optional payload bytes.
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

/// Update the prompt when a complete line is received from tty.
///
/// The tty sends line-buffered input; we only need to react to newline markers.
fn handle_line_payload(stdout: usize, payload: &[u8]) -> Result<()> {
    // Print a new prompt after each completed line.
    if payload.iter().any(|byte| *byte == b'\n') {
        print_prompt(stdout)?;
    }
    Ok(())
}
