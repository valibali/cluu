//! /bin/kill — send a signal to a process.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::debug_print;
use libcluu::posix::_write;

fn spec() -> Spec {
    Spec::new()
        .program("kill")
        .version("0.1.0")
        .usage("[-s SIGNAL | -N] PID...")
        .required('s', "signal", "signal name or number (default: TERM)")
        .flag('l', "list", "list signal names")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

/// Convert a signal name or number string to signal number.
fn parse_signal(s: &str) -> Option<i32> {
    // Try numeric first.
    if let Ok(n) = s.parse::<i32>() {
        return Some(n);
    }
    // Strip "SIG" prefix (case-insensitive).
    let upper = s.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(upper.as_str());
    let n = match name {
        "HUP"  => 1,
        "INT"  => 2,
        "QUIT" => 3,
        "KILL" => 9,
        "TERM" => 15,
        "CONT" => 18,
        "STOP" => 19,
        "USR1" => 10,
        "USR2" => 12,
        "PIPE" => 13,
        "ALRM" => 14,
        "CHLD" => 17,
        _      => return None,
    };
    Some(n)
}

fn do_kill(pid: i32, sig: i32) -> i32 {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid, sig) }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv: Vec<String> = libcluu::args::args();
    let sp = spec();
    let parsed = match parse(&sp, &argv) {
        Ok(p) => p,
        Err(CliError::HelpRequested) => {
            write_fd(1, render_help(&sp).as_bytes());
            let _ = debug_print("kill: ok (exit 0)");
            return 0;
        }
        Err(CliError::VersionRequested) => {
            write_fd(1, b"kill 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("kill: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    // --list: print known signal names.
    if parsed.is_set("list") {
        write_fd(
            1,
            b"HUP INT QUIT KILL TERM CONT STOP USR1 USR2 PIPE ALRM CHLD\n",
        );
        let _ = debug_print("kill: ok (exit 0)");
        return 0;
    }

    // Determine signal number from -s option or default SIGTERM.
    let sig = if let Some(sig_str) = parsed.value("signal") {
        match parse_signal(sig_str) {
            Some(n) => n,
            None => {
                let msg = format!("kill: invalid signal '{}'\n", sig_str);
                write_fd(2, msg.as_bytes());
                return 2;
            }
        }
    } else {
        15 // SIGTERM
    };

    if parsed.positional.is_empty() {
        write_fd(2, b"kill: missing PID operand\n");
        return 2;
    }

    let mut exit_code = 0i32;
    for pid_str in &parsed.positional {
        // Reject job specs — those go to the shell builtin.
        if pid_str.starts_with('%') {
            let msg = format!(
                "kill: %N job specs are supported via shell builtin only\n"
            );
            write_fd(2, msg.as_bytes());
            exit_code = 1;
            continue;
        }

        let pid: i32 = match pid_str.parse() {
            Ok(n) => n,
            Err(_) => {
                let msg = format!("kill: invalid PID '{}'\n", pid_str);
                write_fd(2, msg.as_bytes());
                exit_code = 1;
                continue;
            }
        };

        let r = do_kill(pid, sig);
        if r != 0 {
            let msg = format!("kill: ({}) - No such process\n", pid);
            write_fd(2, msg.as_bytes());
            exit_code = 1;
        }
    }
    let _ = debug_print(&format!("kill: ok (exit {})", exit_code));
    exit_code
}
