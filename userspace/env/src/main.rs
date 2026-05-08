//! /bin/env — run a program in a modified environment, or print environment.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::string::String;
use alloc::vec::Vec;
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::debug_print;
use libcluu::posix::_write;

fn spec() -> Spec {
    Spec::new()
        .program("env")
        .version("0.1.0")
        .usage("[NAME=VALUE...] [COMMAND [ARGS]...]")
        .flag('i', "ignore-environment", "start with empty environment")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

fn print_environ() {
    extern "C" {
        static environ: *const *const u8;
    }
    unsafe {
        let mut p = environ;
        if p.is_null() {
            return;
        }
        while !(*p).is_null() {
            let mut len = 0;
            while *(*p).add(len) != 0 {
                len += 1;
            }
            let bytes = core::slice::from_raw_parts(*p, len);
            if let Ok(s) = core::str::from_utf8(bytes) {
                write_fd(1, s.as_bytes());
                write_fd(1, b"\n");
            }
            p = p.add(1);
        }
    }
}

fn do_setenv(name: &str, val: &str) -> i32 {
    extern "C" {
        fn setenv(name: *const u8, value: *const u8, overwrite: i32) -> i32;
    }
    let mut k = String::from(name);
    k.push('\0');
    let mut v = String::from(val);
    v.push('\0');
    unsafe { setenv(k.as_ptr(), v.as_ptr(), 1) }
}

fn do_unsetenv(name: &str) -> i32 {
    extern "C" {
        fn unsetenv(name: *const u8) -> i32;
    }
    let mut k = String::from(name);
    k.push('\0');
    unsafe { unsetenv(k.as_ptr()) }
}

/// Collect all current env var names, then unset each one.
fn clear_environment() {
    extern "C" {
        static environ: *const *const u8;
    }
    // Collect names first (can't mutate while iterating).
    let mut names: Vec<String> = Vec::new();
    unsafe {
        let mut p = environ;
        if p.is_null() {
            return;
        }
        while !(*p).is_null() {
            let mut len = 0;
            while *(*p).add(len) != 0 {
                len += 1;
            }
            let bytes = core::slice::from_raw_parts(*p, len);
            if let Ok(s) = core::str::from_utf8(bytes) {
                // Extract just the key (up to first '=').
                let key = match s.find('=') {
                    Some(pos) => &s[..pos],
                    None => s,
                };
                names.push(String::from(key));
            }
            p = p.add(1);
        }
    }
    for name in &names {
        do_unsetenv(name);
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv: Vec<String> = libcluu::args::args();
    let sp = spec();
    let parsed = match parse(&sp, &argv) {
        Ok(p) => p,
        Err(CliError::HelpRequested) => {
            write_fd(1, render_help(&sp).as_bytes());
            return 0;
        }
        Err(CliError::VersionRequested) => {
            write_fd(1, b"env 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = alloc::format!("env: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.is_set("ignore-environment") {
        clear_environment();
    }

    // Split positionals into KEY=VAL assignments and optional command.
    let mut cmd_idx = parsed.positional.len();
    for (idx, p) in parsed.positional.iter().enumerate() {
        if let Some(eq_pos) = p.find('=') {
            // Valid env var name before '=': alpha/digit/underscore, non-empty.
            if !p[..eq_pos].is_empty()
                && p[..eq_pos]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
        }
        // Not an assignment — this is the start of the command.
        cmd_idx = idx;
        break;
    }

    // Apply assignments.
    for p in &parsed.positional[..cmd_idx] {
        if let Some(eq_pos) = p.find('=') {
            do_setenv(&p[..eq_pos], &p[eq_pos + 1..]);
        }
    }

    if cmd_idx == parsed.positional.len() {
        // No command — print the environment.
        print_environ();
        let _ = debug_print("env: ok (exit 0)");
        return 0;
    }

    // COMMAND execution: not supported in CLUU (execvp is ENOSYS).
    // env with command args is used as an interpreter shebang wrapper;
    // for now, report unsupported.
    let prog = &parsed.positional[cmd_idx];
    let msg = alloc::format!(
        "env: exec not supported in CLUU; cannot run '{}'\n",
        prog
    );
    write_fd(2, msg.as_bytes());
    127
}
