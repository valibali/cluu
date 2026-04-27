//! `/bin/grep` — print lines matching a literal PATTERN.
//!
//! Usage: grep [-n] [-i] [-v] PATTERN [FILE]
//!
//! With FILE: opens the file via VFS and searches it.
//! Without FILE: reads fd 0 (stdin) until EOF.
//!
//! Flags:
//!   -n  prefix each matched line with its 1-based line number
//!   -i  case-insensitive matching
//!   -v  invert — print lines that do NOT contain PATTERN
//!
//! Exit code: 0 if at least one line matched/printed, 1 if none, 2 on error.

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

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let mut iter = args.into_iter().skip(1);

    let mut show_line_no = false;
    let mut case_insensitive = false;
    let mut invert = false;
    let mut pattern: Option<String> = None;

    for arg in &mut iter {
        match arg.as_str() {
            "-n" => show_line_no = true,
            "-i" => case_insensitive = true,
            "-v" => invert = true,
            s if s.starts_with('-') => {
                let msg = format!("grep: unknown flag {}\n", s);
                write_fd(2, msg.as_bytes());
                return 2;
            }
            _ => {
                pattern = Some(arg);
                break;
            }
        }
    }

    let Some(pattern) = pattern else {
        write_fd(2, b"grep: usage: grep [-n] [-i] [-v] PATTERN [FILE]\n");
        return 2;
    };

    let needle: String = if case_insensitive {
        pattern.to_lowercase()
    } else {
        pattern
    };

    // Optional FILE operand — anything remaining after the pattern.
    let file_path = iter.next();

    // Read the entire source into a heap buffer.
    let mut buf: Vec<u8> = Vec::new();
    if let Some(path) = file_path {
        if read_whole_file_into(&path, &mut buf).is_err() {
            let msg = format!("grep: {}: cannot read\n", path);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    } else {
        // Read from fd 0 (stdin) until EOF.
        let mut chunk = [0u8; CHUNK_SIZE];
        loop {
            let n = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len());
            if n <= 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n as usize]);
        }
    }

    let text = match core::str::from_utf8(&buf) {
        Ok(s) => s,
        Err(_) => {
            write_fd(2, b"grep: input is not valid UTF-8\n");
            return 2;
        }
    };

    let mut any_match = false;
    for (lineno, line) in text.lines().enumerate() {
        let matched = if case_insensitive {
            let hay = line.to_lowercase();
            hay.contains(needle.as_str())
        } else {
            line.contains(needle.as_str())
        };

        let keep = matched ^ invert;
        if keep {
            any_match = true;
            let line_out = if show_line_no {
                format!("{}:{}\n", lineno + 1, line)
            } else {
                format!("{}\n", line)
            };
            write_fd(1, line_out.as_bytes());
        }
    }

    if any_match { 0 } else { 1 }
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
