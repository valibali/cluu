#![no_std]
#![no_main]

extern crate alloc;

#[cfg(feature = "lang-parser")]
mod commands;

use alloc::format;
#[cfg(feature = "lang-parser")]
use alloc::string::String;
#[cfg(feature = "lang-parser")]
use alloc::string::ToString;
#[cfg(feature = "lang-parser")]
use alloc::vec::Vec;
#[cfg(feature = "lang-parser")]
use commands::{BuiltinFactory, CommandContext, CommandExecutor, ExecResult};
use core::mem::size_of;
use libcluu::boot::{
    process_info, PARAM_ARGC, PARAM_ARGV_OFFSET, TOKEN_STDERR, TOKEN_STDIN, TOKEN_STDLOG,
};
use libcluu::ipc::{
    send_with_payload, CONSOLE_CLEAR_LABEL, CONSOLE_WRITE_LABEL, TTY_READ_LABEL, TTY_WRITE_LABEL,
};
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
        match registry::subscribe_output("tty:0", "main") {
            Ok(token) => break token,
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    };
    let console_write = loop {
        // Best-effort console access for clearing before banner.
        match registry::subscribe_output("console:0", "write") {
            Ok(token) => break token,
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    };
    let registry_endpoint = registry::control_endpoint();
    let mut command_context = CommandContext::new();

    debug_print("shell: ready")?;
    let _ = debug_print(&format!(
        "shell: stdin {} stdout {} stderr {} stdlog {}",
        stdin, stdout, stderr, stdlog
    ));
    let _ = send_with_payload(console_write, CONSOLE_CLEAR_LABEL, &[]);
    let _ = print_banner(console_write);
    let _ = print_prompt(stdout);
    #[cfg(feature = "lang-parser")]
    {
        let info = process_info();
        let _ = debug_print(&format!(
            "shell: startup argc={} argv_offset={}",
            info.params[PARAM_ARGC], info.params[PARAM_ARGV_OFFSET]
        ));
        if let Some(startup_cmd) = startup_command_from_process_info() {
            let _ = debug_print(&format!("shell: startup command '{}'", startup_cmd));
            let mut line = startup_cmd;
            line.push('\n');
            let _ = parse_and_execute_line(stdout, stdlog, &mut command_context, line.as_bytes());
            let _ = print_prompt(stdout);
        } else {
            let _ = debug_print("shell: no startup command");
        }
    }

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
                            handle_line_payload(stdout, stdlog, &mut command_context, payload)?;
                        } else if msg.tag.words >= 2 {
                            let ch = msg.words[1] as u8;
                            handle_line_payload(stdout, stdlog, &mut command_context, &[ch])?;
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

#[cfg(feature = "lang-parser")]
fn startup_command_from_process_info() -> Option<String> {
    let info = process_info();
    let argc = info.params[PARAM_ARGC] as usize;
    let argv_offset = info.params[PARAM_ARGV_OFFSET] as usize;
    if argc <= 1 || argv_offset == 0 {
        return None;
    }

    let info_page_base = libcluu::boot::PROCESS_INFO_ADDR & !0xfff;
    let page = unsafe { core::slice::from_raw_parts(info_page_base as *const u8, 4096) };
    if argv_offset >= page.len() {
        return None;
    }
    let mut cursor = argv_offset;
    let mut argv = Vec::new();
    for _ in 0..argc {
        if cursor >= page.len() {
            break;
        }
        let start = cursor;
        while cursor < page.len() && page[cursor] != 0 {
            cursor += 1;
        }
        if cursor > start {
            if let Ok(text) = core::str::from_utf8(&page[start..cursor]) {
                argv.push(text);
            }
        }
        if cursor < page.len() {
            cursor += 1;
        }
    }
    if argv.len() <= 1 {
        return None;
    }
    let mut cmd = String::new();
    for (idx, arg) in argv.iter().enumerate().skip(1) {
        if idx > 1 {
            cmd.push(' ');
        }
        cmd.push_str(arg);
    }
    if cmd.is_empty() {
        None
    } else {
        Some(cmd)
    }
}

fn print_banner(console_endpoint: usize) -> Result<()> {
    // ASCII-only banner, stored in a separate file for easy editing.
    const BANNER: &str = include_str!("banner.txt");
    // Send per line to avoid splitting UTF-8 sequences across IPC messages.
    for line in BANNER.lines() {
        send_with_payload(console_endpoint, CONSOLE_WRITE_LABEL, line.as_bytes())?;
        send_with_payload(console_endpoint, CONSOLE_WRITE_LABEL, b"\n")?;
    }
    send_with_payload(console_endpoint, CONSOLE_WRITE_LABEL, b"\n")?;
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
fn handle_line_payload(
    stdout: usize,
    stdlog: usize,
    context: &mut CommandContext,
    payload: &[u8],
) -> Result<()> {
    #[cfg(not(feature = "lang-parser"))]
    let _ = (stdlog, context);
    // Print a new prompt after each completed line.
    if payload.contains(&b'\n') {
        #[cfg(feature = "lang-parser")]
        {
            parse_and_execute_line(stdout, stdlog, context, payload)?;
        }
        print_prompt(stdout)?;
    }
    Ok(())
}

#[cfg(feature = "lang-parser")]
fn parse_and_execute_line(
    stdout: usize,
    stdlog: usize,
    context: &mut CommandContext,
    payload: &[u8],
) -> Result<()> {
    let line = strip_trailing_newline(payload);
    match core::str::from_utf8(line) {
        Ok(text) => match cluu_lang::parse_program(text) {
            Ok(ast) => {
                let factory = BuiltinFactory::new();
                let registry = factory.build();
                match registry.execute(stdout, context, &ast) {
                    Ok(ExecResult::Handled) => return Ok(()),
                    Ok(ExecResult::NotHandled) => {}
                    Err(err) => {
                        let _ =
                            send_with_payload(stdlog, TTY_WRITE_LABEL, err.to_string().as_bytes());
                        let _ = debug_print(&format!("shell: builtin error {}", err));
                        return Ok(());
                    }
                }
                let _ = debug_print("shell: unsupported command");
                let _ = send_with_payload(stdlog, TTY_WRITE_LABEL, b"shell: unsupported command\n");
            }
            Err(err) => {
                let _ = send_with_payload(stdlog, TTY_WRITE_LABEL, err.to_string().as_bytes());
                let _ = debug_print(&format!("shell: parse error {}", err));
            }
        },
        Err(_) => {
            let _ = send_with_payload(stdlog, TTY_WRITE_LABEL, b"shell: invalid utf-8\n");
            let _ = debug_print("shell: invalid utf-8");
        }
    }
    Ok(())
}

#[cfg(feature = "lang-parser")]
fn strip_trailing_newline(payload: &[u8]) -> &[u8] {
    let mut end = payload.len();
    if end > 0 && payload[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && payload[end - 1] == b'\r' {
        end -= 1;
    }
    &payload[..end]
}
