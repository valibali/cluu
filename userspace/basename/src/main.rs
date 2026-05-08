//! /bin/basename — strip directory and optional suffix from a path.

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
        .program("basename")
        .version("0.1.0")
        .usage("PATH [SUFFIX]")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
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
            write_fd(1, b"basename 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = alloc::format!("basename: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.is_empty() {
        write_fd(2, b"basename: missing operand\n");
        return 2;
    }

    let path = &parsed.positional[0];
    let suffix = parsed.positional.get(1).map(|s| s.as_str()).unwrap_or("");

    // Strip trailing slashes, then take last component.
    let trimmed = path.trim_end_matches('/');
    let base = if trimmed.is_empty() {
        "/"
    } else {
        match trimmed.rfind('/') {
            Some(p) => &trimmed[p + 1..],
            None => trimmed,
        }
    };

    // Strip suffix if provided and present (but not if it would leave empty string).
    let out = if !suffix.is_empty() && base.ends_with(suffix) && base.len() > suffix.len() {
        &base[..base.len() - suffix.len()]
    } else {
        base
    };

    write_fd(1, out.as_bytes());
    write_fd(1, b"\n");
    let _ = debug_print("basename: ok (exit 0)");
    0
}
