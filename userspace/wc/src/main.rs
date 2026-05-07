//! `/bin/wc` — count lines, words, and bytes.
//!
//! Usage: wc [-l] [-w] [-c] [FILE]
//!
//! With FILE: opens the file via VFS.
//! Without FILE: reads fd 0 (stdin) until EOF.
//!
//! Flags:
//!   -l  count newline bytes (lines)
//!   -w  count whitespace-separated tokens (words)
//!   -c  count bytes
//!   (no flags) print all three in order: lines words bytes
//!
//! Output format matches GNU wc: each count right-aligned in 7 columns,
//! separated by spaces, followed by an optional filename, then newline.
//!
//! Exit code: 0 on success, 1 on file read error, 2 on usage error.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_read, _write};
use libcluu::registry;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let mut iter = args.into_iter().skip(1);

    let mut want_l = false;
    let mut want_w = false;
    let mut want_c = false;
    let mut path: Option<String> = None;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-l" => want_l = true,
            "-w" => want_w = true,
            "-c" => want_c = true,
            s if s.starts_with('-') => {
                let line = format!("wc: unknown flag {}\n", s);
                write_fd(2, line.as_bytes());
                return 2;
            }
            _ => {
                path = Some(arg);
                break;
            }
        }
    }

    if !want_l && !want_w && !want_c {
        want_l = true;
        want_w = true;
        want_c = true;
    }

    let mut buf: Vec<u8> = Vec::new();
    let displayed_path = path.clone();
    if let Some(p) = path {
        if read_whole_file_into(&p, &mut buf).is_err() {
            let line = format!("wc: {}: cannot read\n", p);
            write_fd(2, line.as_bytes());
            return 1;
        }
    } else {
        let mut chunk = [0u8; 4096];
        loop {
            let r = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len());
            if r <= 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..r as usize]);
        }
    }

    let lines = buf.iter().filter(|&&b| b == b'\n').count();
    let words = match core::str::from_utf8(&buf) {
        Ok(s) => s.split_whitespace().count(),
        Err(_) => 0,
    };
    let bytes = buf.len();

    let mut out = String::new();
    if want_l {
        out.push_str(&format!(" {:>7}", lines));
    }
    if want_w {
        out.push_str(&format!(" {:>7}", words));
    }
    if want_c {
        out.push_str(&format!(" {:>7}", bytes));
    }
    if let Some(p) = displayed_path {
        out.push(' ');
        out.push_str(&p);
    }
    out.push('\n');

    write_fd(1, out.as_bytes());
    0
}

/// Read the entire contents of `path` into `dst` via VFS.
fn read_whole_file_into(path: &str, dst: &mut Vec<u8>) -> Result<(), ()> {
    use libcluu::boot::{process_info, TOKEN_SPACE};

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
    const CHUNK: usize = 64 * 1024;
    let chunk_alloc = ((CHUNK.min(total)) + 4095) & !4095;
    let scratch_base = libcluu::vspace::VSPACE
        .lock()
        .alloc(chunk_alloc)
        .map_err(|_| {
            let _ = vfs.close(file);
        })?;

    let mut offset = 0usize;
    let mut result: Result<(), ()> = Ok(());
    while offset < total {
        let want = (total - offset).min(CHUNK);
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
