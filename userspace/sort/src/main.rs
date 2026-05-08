//! /bin/sort — sort lines of text.

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
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_read, _write};
use libcluu::registry;

fn spec() -> Spec {
    Spec::new()
        .program("sort")
        .version("0.1.0")
        .usage("[-nru] [FILE]")
        .flag('n', "numeric-sort", "compare according to string numerical value")
        .flag('r', "reverse", "reverse the result of comparisons")
        .flag('u', "unique", "with -c, check for strict ordering; without -c, output only the first of an equal run")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

fn read_whole_file_into(vfs: &VfsClient, path: &str, dst: &mut Vec<u8>) -> Result<(), ()> {
    use libcluu::boot::{process_info, TOKEN_SPACE};
    const CHUNK: usize = 64 * 1024;

    let file = vfs.open(path).map_err(|_| ())?;
    let total = file.size;
    if total == 0 {
        let _ = vfs.close(file);
        return Ok(());
    }
    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let chunk_alloc = ((CHUNK.min(total)) + 4095) & !4095;
    let scratch_base = libcluu::vspace::VSPACE
        .lock()
        .alloc(chunk_alloc)
        .map_err(|_| { let _ = vfs.close(file); })?;

    let mut offset = 0usize;
    let mut result: Result<(), ()> = Ok(());
    while offset < total {
        let want = (total - offset).min(CHUNK);
        match vfs.read_grant(file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 { break; }
                let slice = unsafe {
                    core::slice::from_raw_parts(scratch_base as *const u8, grant.len)
                };
                dst.extend_from_slice(slice);
                offset += grant.len;
            }
            Err(_) => { result = Err(()); break; }
        }
    }
    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, chunk_alloc);
    let _ = vfs.close(file);
    result
}

/// Parse the leading integer from a string for numeric sort.
/// Lines that don't start with a digit sort numerically as i64::MIN.
fn leading_int(s: &str) -> i64 {
    let s = s.trim_start();
    let (neg, s) = if s.starts_with('-') { (true, &s[1..]) } else { (false, s) };
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return i64::MIN;
    }
    let val: i64 = digits.parse().unwrap_or(i64::MIN);
    if neg { -val } else { val }
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
            write_fd(1, b"sort 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("sort: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    let numeric = parsed.is_set("numeric-sort");
    let reverse = parsed.is_set("reverse");
    let unique = parsed.is_set("unique");

    let text: String = if let Some(path) = parsed.positional.first() {
        let Ok(vfs_ep) = registry::subscribe_output("vfs", "main") else {
            write_fd(2, b"sort: vfs unavailable\n");
            return 1;
        };
        let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());
        let resolved = libcluu::posix::resolve_path(path);
        let mut buf: Vec<u8> = Vec::new();
        if read_whole_file_into(&vfs, &resolved, &mut buf).is_err() {
            let msg = format!("sort: {}: cannot read\n", path);
            write_fd(2, msg.as_bytes());
            return 1;
        }
        match alloc::string::String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => {
                write_fd(2, b"sort: input not valid UTF-8\n");
                return 1;
            }
        }
    } else {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let r = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len());
            if r <= 0 { break; }
            buf.extend_from_slice(&chunk[..r as usize]);
        }
        match alloc::string::String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => {
                write_fd(2, b"sort: input not valid UTF-8\n");
                return 1;
            }
        }
    };

    let mut lines: Vec<String> = text.lines().map(|l| String::from(l)).collect();

    if numeric {
        lines.sort_by(|a, b| leading_int(a).cmp(&leading_int(b)));
    } else {
        lines.sort();
    }

    if reverse {
        lines.reverse();
    }

    if unique {
        lines.dedup();
    }

    for line in &lines {
        let mut out = String::from(line.as_str());
        out.push('\n');
        write_fd(1, out.as_bytes());
    }

    let _ = debug_print("sort: ok (exit 0)");
    0
}
