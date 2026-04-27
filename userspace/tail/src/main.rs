//! `/bin/tail` — print last N lines of input.
//!
//! Usage: tail [-n N] [FILE]
//!
//! With FILE: opens the file via VFS.
//! Without FILE: reads fd 0 (stdin) until EOF.
//!
//! Flags:
//!   -n N  print last N lines (default 10); joined form -nN also accepted
//!   -N    BSD-style shorthand (e.g. tail -3)
//!
//! Exit code: 0 on success, 1 on read error, 2 on usage error.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_read, _write};
use libcluu::registry;

const CHUNK_SIZE: usize = 64 * 1024;
// Stdin reads use a stack-allocated buffer; keep it small to avoid stack overflow.
const STDIN_CHUNK: usize = 4 * 1024;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let mut iter = args.into_iter().skip(1);

    let mut n: usize = 10;
    let mut path: Option<String> = None;

    while let Some(arg) = iter.next() {
        if arg == "-n" {
            let v = match iter.next() {
                Some(v) => v,
                None => return usage_err(),
            };
            match v.parse::<usize>() {
                Ok(parsed) => n = parsed,
                Err(_) => return usage_err(),
            }
        } else if arg.starts_with("-n") {
            match arg[2..].parse::<usize>() {
                Ok(parsed) => n = parsed,
                Err(_) => return usage_err(),
            }
        } else if arg.len() >= 2 && arg[1..].chars().all(|c| c.is_ascii_digit()) {
            // BSD-style: tail -N  (e.g. tail -3)
            match arg[1..].parse::<usize>() {
                Ok(parsed) => n = parsed,
                Err(_) => return usage_err(),
            }
        } else if arg.starts_with('-') {
            return usage_err();
        } else {
            path = Some(arg);
            break;
        }
    }

    let mut buf: Vec<u8> = Vec::new();
    if let Some(p) = path {
        if read_whole_file_into(&p, &mut buf).is_err() {
            let msg = format!("tail: {}: cannot read\n", p);
            write_fd(2, msg.as_bytes());
            return 1;
        }
    } else {
        let mut chunk = [0u8; STDIN_CHUNK];
        loop {
            let r = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len());
            if r <= 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..r as usize]);
        }
    }

    let text = match core::str::from_utf8(&buf) {
        Ok(s) => s,
        Err(_) => return 1,
    };

    // Collect lines, then emit the last N.
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    for line in &lines[start..] {
        let line_out = format!("{}\n", line);
        write_fd(1, line_out.as_bytes());
    }
    0
}

fn usage_err() -> i32 {
    write_fd(2, b"tail: usage: tail [-n N] [FILE]\n");
    2
}

/// Read the entire contents of `path` into `dst` via VFS.
fn read_whole_file_into(path: &str, dst: &mut Vec<u8>) -> Result<(), ()> {
    let resolved = libcluu::posix::resolve_path(path);
    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        return Err(());
    };
    let vfs = VfsClient::new(vfs_endpoint, registry::control_endpoint());

    let file = vfs.open(&resolved).map_err(|_| ())?;
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
