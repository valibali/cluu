//! Minimal TTY service (echo + stdin forwarding).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, vec::Vec};
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};
use libcluu::boot::{process_info, TOKEN_PROC_CAP};
use libcluu::ipc::{
    send_with_payload, CONSOLE_WRITE_LABEL, KBD_EVENT_LABEL, TTY_READ_LABEL, TTY_REGISTER_LABEL,
    TTY_WRITE_LABEL,
};
use libcluu::registry;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Error, Result};

// Token indices (set by init).
const SVC_TOKEN_LISTEN: usize = 6;

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
    let proc_cap = info.tokens[TOKEN_PROC_CAP];
    // Prefer a fresh endpoint created from proc_cap so tty can grant send-only
    // tokens to subscribers via the registry.
    let endpoint = if proc_cap != 0 {
        match libcluu::syscall::endpoint_create(proc_cap) {
            Ok(token) => token,
            Err(_) => info.tokens[SVC_TOKEN_LISTEN],
        }
    } else {
        info.tokens[SVC_TOKEN_LISTEN]
    };
    registry::init("tty")?;
    registry::register_default_outputs()?;
    // Expose the tty input for kbd/shell subscriptions.
    registry::register_output("main", endpoint)?;
    let registry_endpoint = registry::control_endpoint();

    debug_print(&format!(
        "tty: endpoint {} registry {}",
        endpoint, registry_endpoint
    ))?;
    debug_print("tty: ready")?;
    yield_cpu()?;

    // Subscribed endpoints (populated asynchronously via registry grants).
    let mut shell_stdin: usize = 0;
    let mut console_endpoint: usize = 0;
    let mut requested_console = false;
    let mut requested_shell = false;
    let mut line: Vec<u8> = Vec::new();
    // Buffer outbound console text until we have a console subscription.
    let mut pending_console_output: Vec<u8> = Vec::new();
    let mut saw_key = false;

    let mut buf = [0u8; 256];
    loop {
        // Request console and shell subscriptions lazily; retry on failure.
        if console_endpoint == 0
            && !requested_console
            && registry::request_subscription("console", "write").is_ok()
        {
            requested_console = true;
        }
        if shell_stdin == 0
            && !requested_shell
            && registry::request_subscription("shell", "stdin").is_ok()
        {
            requested_shell = true;
        }

        let tokens = [endpoint, registry_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((index, len)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    if index == 1 {
                        // Registry control traffic (grants and subscribe status).
                        if let Ok(Some(event)) = registry::handle_incoming_message(&msg, payload) {
                            match event {
                                registry::RegistryEvent::Grant { name, token } => {
                                    if name == "write" {
                                        console_endpoint = token;
                                        let _ = debug_print("tty: console subscribed");
                                        // Flush any early shell output that arrived before we
                                        // had a console endpoint.
                                        if !pending_console_output.is_empty() {
                                            let _ = send_with_payload(
                                                console_endpoint,
                                                CONSOLE_WRITE_LABEL,
                                                &pending_console_output,
                                            );
                                            pending_console_output.clear();
                                        }
                                    } else if name == "stdin" {
                                        shell_stdin = token;
                                        let _ = debug_print("tty: shell stdin subscribed");
                                    }
                                }
                                registry::RegistryEvent::SubscribeStatus { code } => {
                                    // Retry on failure (registry replies with non-zero status).
                                    if code != 0 {
                                        requested_console = false;
                                        requested_shell = false;
                                    }
                                }
                            }
                        }
                        continue;
                    }
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
                            if console_endpoint != 0 {
                                let _ = send_with_payload(
                                    console_endpoint,
                                    CONSOLE_WRITE_LABEL,
                                    payload,
                                );
                            } else if pending_console_output.len() + payload.len() <= 2048 {
                                // Keep a small buffer so the initial shell prompt is not lost.
                                pending_console_output.extend_from_slice(payload);
                            }
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
    // IPC messages are header + payload; clamp payload if malformed.
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
    let msg = Message::new(TTY_READ_LABEL, [0, ch as usize, 0, 0, 0, 0], 2);
    let _ = libcluu::ipc::send(shell_stdin, &msg, IpcFlags::empty());
}
