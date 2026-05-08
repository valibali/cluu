//! /bin/dirname — strip last component from a path.

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
        .program("dirname")
        .version("0.1.0")
        .usage("PATH")
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
            write_fd(1, b"dirname 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = alloc::format!("dirname: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.is_empty() {
        write_fd(2, b"dirname: missing operand\n");
        return 2;
    }

    let path = &parsed.positional[0];

    // Strip trailing slashes (unless the path is purely slashes).
    let trimmed = path.trim_end_matches('/');
    let dir = if trimmed.is_empty() {
        // Input was "/" or all slashes.
        "/"
    } else {
        match trimmed.rfind('/') {
            Some(0) => "/",
            Some(p) => &trimmed[..p],
            None => ".",
        }
    };

    write_fd(1, dir.as_bytes());
    write_fd(1, b"\n");
    let _ = debug_print("dirname: ok (exit 0)");
    0
}
