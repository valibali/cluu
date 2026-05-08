//! /bin/uniq — filter adjacent duplicate lines.

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
        .program("uniq")
        .version("0.1.0")
        .usage("[-cd] [INPUT]")
        .flag('c', "count", "prefix lines by number of occurrences")
        .flag('d', "repeated", "only print duplicate lines")
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

fn process(text: &str, count: bool, repeated: bool) {
    let mut prev: Option<&str> = None;
    let mut run = 0usize;

    // We need to iterate and flush on change.
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();

    for (i, line) in lines.iter().enumerate() {
        let changed = prev.map_or(true, |p| p != *line);
        if changed {
            // Flush previous run.
            if let Some(p) = prev {
                let should_print = !repeated || run > 1;
                if should_print {
                    let out = if count {
                        format!("{:>4} {}\n", run, p)
                    } else {
                        format!("{}\n", p)
                    };
                    write_fd(1, out.as_bytes());
                }
            }
            prev = Some(line);
            run = 1;
        } else {
            run += 1;
        }

        // Flush last run at end.
        if i + 1 == n {
            let should_print = !repeated || run > 1;
            if should_print {
                let out = if count {
                    format!("{:>4} {}\n", run, line)
                } else {
                    format!("{}\n", line)
                };
                write_fd(1, out.as_bytes());
            }
        }
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
            write_fd(1, b"uniq 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("uniq: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    let count = parsed.is_set("count");
    let repeated = parsed.is_set("repeated");

    let text: String = if let Some(path) = parsed.positional.first() {
        let Ok(vfs_ep) = registry::subscribe_output("vfs", "main") else {
            write_fd(2, b"uniq: vfs unavailable\n");
            return 1;
        };
        let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());
        let resolved = libcluu::posix::resolve_path(path);
        let mut buf: Vec<u8> = Vec::new();
        if read_whole_file_into(&vfs, &resolved, &mut buf).is_err() {
            let msg = format!("uniq: {}: cannot read\n", path);
            write_fd(2, msg.as_bytes());
            return 1;
        }
        match alloc::string::String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => {
                write_fd(2, b"uniq: input not valid UTF-8\n");
                return 1;
            }
        }
    } else {
        // Read from stdin.
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
                write_fd(2, b"uniq: input not valid UTF-8\n");
                return 1;
            }
        }
    };

    process(&text, count, repeated);
    let _ = debug_print("uniq: ok (exit 0)");
    0
}
