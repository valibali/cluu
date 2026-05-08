//! /bin/cut — remove sections from each line of files.

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
        .program("cut")
        .version("0.1.0")
        .usage("[-f LIST -d DELIM | -c LIST] [FILE]")
        .required('f', "fields", "select only these fields (1-based, comma-sep, ranges OK)")
        .required('d', "delimiter", "use DELIM instead of TAB for field delimiter")
        .required('c', "characters", "select only these character positions")
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

/// Parse a LIST string like "1,3-5,7" into sorted unique 0-based indices.
/// Returns an empty vec on parse error.
fn parse_list(s: &str) -> Vec<usize> {
    let mut indices: Vec<usize> = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if let Some(dash) = part.find('-') {
            let lo: usize = part[..dash].parse().unwrap_or(1);
            let hi: usize = part[dash + 1..].parse().unwrap_or(lo);
            for i in lo..=hi {
                if i >= 1 { indices.push(i - 1); }
            }
        } else {
            let n: usize = part.parse().unwrap_or(0);
            if n >= 1 { indices.push(n - 1); }
        }
    }
    indices.sort();
    indices.dedup();
    indices
}

fn cut_fields(line: &str, delim: char, indices: &[usize]) -> String {
    let fields: Vec<&str> = line.split(delim).collect();
    let mut out = String::new();
    let mut first = true;
    for &idx in indices {
        if idx < fields.len() {
            if !first { out.push(delim); }
            out.push_str(fields[idx]);
            first = false;
        }
    }
    out
}

fn cut_chars(line: &str, indices: &[usize]) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    for &idx in indices {
        if idx < chars.len() {
            out.push(chars[idx]);
        }
    }
    out
}

fn process_text(text: &str, fields: Option<(&str, char)>, chars: Option<&str>) {
    for line in text.lines() {
        let result = if let Some((list, delim)) = fields {
            let indices = parse_list(list);
            cut_fields(line, delim, &indices)
        } else if let Some(list) = chars {
            let indices = parse_list(list);
            cut_chars(line, &indices)
        } else {
            String::from(line)
        };
        let mut out = result;
        out.push('\n');
        write_fd(1, out.as_bytes());
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
            write_fd(1, b"cut 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("cut: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    let fields_list = parsed.value("fields");
    let delim_str = parsed.value("delimiter").unwrap_or("\t");
    let chars_list = parsed.value("characters");

    if fields_list.is_none() && chars_list.is_none() {
        write_fd(2, b"cut: you must specify a list of bytes, characters, or fields\n");
        return 2;
    }

    let delim: char = delim_str.chars().next().unwrap_or('\t');

    let fields_arg: Option<(&str, char)> = fields_list.map(|f| (f, delim));

    let text: String = if let Some(path) = parsed.positional.first() {
        let Ok(vfs_ep) = registry::subscribe_output("vfs", "main") else {
            write_fd(2, b"cut: vfs unavailable\n");
            return 1;
        };
        let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());
        let resolved = libcluu::posix::resolve_path(path);
        let mut buf: Vec<u8> = Vec::new();
        if read_whole_file_into(&vfs, &resolved, &mut buf).is_err() {
            let msg = format!("cut: {}: cannot read\n", path);
            write_fd(2, msg.as_bytes());
            return 1;
        }
        match alloc::string::String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => {
                write_fd(2, b"cut: input not valid UTF-8\n");
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
                write_fd(2, b"cut: input not valid UTF-8\n");
                return 1;
            }
        }
    };

    process_text(&text, fields_arg, chars_list);
    let _ = debug_print("cut: ok (exit 0)");
    0
}
