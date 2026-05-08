//! `/bin/head` — print first N lines or bytes of input.
//!
//! Flags: -n N, -c BYTES, -q, -v; multi-file headers

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_read, _write};
use libcluu::registry;

const CHUNK_SIZE: usize = 64 * 1024;
const STDIN_CHUNK: usize = 4 * 1024;

fn spec() -> Spec {
    Spec::new()
        .program("head")
        .version("0.1.0")
        .usage("[-qv] [-n N] [-c BYTES] [FILE]...")
        .required('n', "lines", "print the first N lines instead of the first 10")
        .required('c', "bytes", "print the first N bytes")
        .flag('q', "quiet", "never print headers giving file names")
        .flag('v', "verbose", "always print headers giving file names")
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let sp = spec();
    let parsed = match parse(&sp, &args) {
        Ok(p) => p,
        Err(CliError::HelpRequested) => {
            let h = render_help(&sp);
            write_fd(1, h.as_bytes());
            return 0;
        }
        Err(CliError::VersionRequested) => {
            write_fd(1, b"head 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("head: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    let n_lines: Option<usize> = parsed.value("lines").and_then(|v| v.parse().ok());
    let n_bytes: Option<usize> = parsed.value("bytes").and_then(|v| v.parse().ok());
    let quiet = parsed.is_set("quiet");
    let always_header = parsed.is_set("verbose");

    // Default: 10 lines if neither -n nor -c specified.
    let mode = if let Some(b) = n_bytes {
        HeadMode::Bytes(b)
    } else {
        HeadMode::Lines(n_lines.unwrap_or(10))
    };

    let multi_file = parsed.positional.len() > 1;

    if parsed.positional.is_empty() {
        // Stdin mode.
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; STDIN_CHUNK];
        loop {
            let r = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len());
            if r <= 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..r as usize]);
        }
        emit_head(&buf, &mode);
        return 0;
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"head: vfs unavailable\n");
        return 1;
    };
    let vfs = VfsClient::new(vfs_endpoint, registry::control_endpoint());

    let mut exit_code = 0i32;
    let mut first = true;
    for path in &parsed.positional {
        // Print header when: multiple files and not -q; or always when -v.
        if !quiet && (multi_file || always_header) {
            if !first {
                write_fd(1, b"\n");
            }
            let hdr = format!("==> {} <==\n", path);
            write_fd(1, hdr.as_bytes());
        }
        first = false;

        let mut buf: Vec<u8> = Vec::new();
        let resolved = libcluu::posix::resolve_path(path);
        if read_whole_file_into(&vfs, &resolved, &mut buf).is_err() {
            let msg = format!("head: {}: cannot read\n", path);
            write_fd(2, msg.as_bytes());
            exit_code = 1;
            continue;
        }
        emit_head(&buf, &mode);
    }
    let _ = debug_print(&format!("head: ok (exit {})", exit_code));
    exit_code
}

enum HeadMode {
    Lines(usize),
    Bytes(usize),
}

fn emit_head(buf: &[u8], mode: &HeadMode) {
    match mode {
        HeadMode::Bytes(n) => {
            let out = &buf[..buf.len().min(*n)];
            write_fd(1, out);
        }
        HeadMode::Lines(n) => {
            let text = match core::str::from_utf8(buf) {
                Ok(s) => s,
                Err(_) => return,
            };
            for (i, line) in text.lines().enumerate() {
                if i >= *n {
                    break;
                }
                let out = format!("{}\n", line);
                write_fd(1, out.as_bytes());
            }
        }
    }
}

fn read_whole_file_into(vfs: &VfsClient, path: &str, dst: &mut Vec<u8>) -> Result<(), ()> {
    let file = vfs.open(path).map_err(|_| ())?;
    let total = file.size;
    if total == 0 {
        let _ = vfs.close(file);
        return Ok(());
    }

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let chunk_alloc = ((CHUNK_SIZE.min(total)) + 4095) & !4095;
    let scratch_base = libcluu::vspace::VSPACE
        .lock()
        .alloc(chunk_alloc)
        .map_err(|_| {
            let _ = vfs.close(file);
        })?;

    let mut offset = 0usize;
    let mut result: Result<(), ()> = Ok(());
    while offset < total {
        let want = (total - offset).min(CHUNK_SIZE);
        match vfs.read_grant(file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let slice = unsafe {
                    core::slice::from_raw_parts(scratch_base as *const u8, grant.len)
                };
                dst.extend_from_slice(slice);
                offset += grant.len;
            }
            Err(_) => {
                result = Err(());
                break;
            }
        }
    }

    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, chunk_alloc);
    let _ = vfs.close(file);
    result
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}
