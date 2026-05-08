//! `/bin/cat` — concatenate files and print to stdout.
//!
//! Flags: -n, -b, -A, -E, -T, -s

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_read, _write};
use libcluu::registry;

const CHUNK_SIZE: usize = 64 * 1024;

fn spec() -> Spec {
    Spec::new()
        .program("cat")
        .version("0.1.0")
        .usage("[-nbAETs] [FILE]...")
        .flag('n', "number", "number all output lines")
        .flag('b', "number-nonblank", "number nonempty output lines (overrides -n)")
        .flag('A', "show-all", "equivalent to -ET")
        .flag('E', "show-ends", "display $ at end of each line")
        .flag('T', "show-tabs", "display TAB characters as ^I")
        .flag('s', "squeeze-blank", "suppress repeated empty output lines")
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
            write_fd(1, b"cat 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("cat: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    // -A = -E -T
    let show_ends = parsed.is_set("show-ends") || parsed.is_set("show-all");
    let show_tabs = parsed.is_set("show-tabs") || parsed.is_set("show-all");
    let number_nonblank = parsed.is_set("number-nonblank");
    let number_all = parsed.is_set("number") && !number_nonblank;
    let squeeze = parsed.is_set("squeeze-blank");

    let needs_transform = show_ends || show_tabs || number_nonblank || number_all || squeeze;

    if parsed.positional.is_empty() {
        if needs_transform {
            return cat_stdin_transform(show_ends, show_tabs, number_nonblank, number_all, squeeze);
        }
        return cat_stdin();
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"cat: vfs unavailable\n");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let vfs = VfsClient::new(vfs_endpoint, client_id);

    let mut exit_code: i32 = 0;
    for path in &parsed.positional {
        let resolved = libcluu::posix::resolve_path(path);
        let result = if needs_transform {
            cat_file_transform(
                &vfs,
                &resolved,
                show_ends,
                show_tabs,
                number_nonblank,
                number_all,
                squeeze,
            )
        } else {
            cat_file(&vfs, &resolved)
        };
        if let Err(reason) = result {
            let line = format!("cat: {}: {}\n", path, reason);
            write_fd(2, line.as_bytes());
            exit_code = 1;
        }
    }
    let _ = debug_print(&format!("cat: ok (exit {})", exit_code));
    exit_code
}

fn cat_stdin() -> i32 {
    let mut buf = [0u8; CHUNK_SIZE];
    loop {
        let n = _read(0, buf.as_mut_ptr() as *mut _, buf.len());
        if n == 0 {
            return 0;
        }
        if n < 0 {
            return 1;
        }
        let m = _write(1, buf.as_ptr() as *const _, n as usize);
        if m < 0 {
            return 1;
        }
    }
}

fn cat_stdin_transform(
    show_ends: bool,
    show_tabs: bool,
    number_nonblank: bool,
    number_all: bool,
    squeeze: bool,
) -> i32 {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let r = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len());
        if r <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..r as usize]);
    }
    let text = match core::str::from_utf8(&buf) {
        Ok(s) => s,
        Err(_) => return 1,
    };
    emit_transformed(text, show_ends, show_tabs, number_nonblank, number_all, squeeze);
    0
}

fn cat_file(vfs: &VfsClient, path: &str) -> Result<(), String> {
    let file = vfs.open(path).map_err(|e| format!("{:?}", e))?;
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
            String::from("out of virtual memory")
        })?;

    let mut offset = 0usize;
    let mut result: Result<(), String> = Ok(());
    while offset < total {
        let remaining = total - offset;
        let want = remaining.min(CHUNK_SIZE);
        match vfs.read_grant(file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let slice = unsafe {
                    core::slice::from_raw_parts(scratch_base as *const u8, grant.len)
                };
                let m = _write(1, slice.as_ptr() as *const _, slice.len());
                if m < 0 {
                    result = Err(String::from("write failed"));
                    break;
                }
                offset += grant.len;
            }
            Err(e) => {
                result = Err(format!("read: {:?}", e));
                break;
            }
        }
    }

    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, chunk_alloc);
    let _ = vfs.close(file);
    result
}

fn cat_file_transform(
    vfs: &VfsClient,
    path: &str,
    show_ends: bool,
    show_tabs: bool,
    number_nonblank: bool,
    number_all: bool,
    squeeze: bool,
) -> Result<(), String> {
    let file = vfs.open(path).map_err(|e| format!("{:?}", e))?;
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
            String::from("out of virtual memory")
        })?;

    let mut raw: Vec<u8> = Vec::new();
    let mut offset = 0usize;
    let mut result: Result<(), String> = Ok(());
    while offset < total {
        let remaining = total - offset;
        let want = remaining.min(CHUNK_SIZE);
        match vfs.read_grant(file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let slice = unsafe {
                    core::slice::from_raw_parts(scratch_base as *const u8, grant.len)
                };
                raw.extend_from_slice(slice);
                offset += grant.len;
            }
            Err(e) => {
                result = Err(format!("read: {:?}", e));
                break;
            }
        }
    }

    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, chunk_alloc);
    let _ = vfs.close(file);
    result?;

    let text = match core::str::from_utf8(&raw) {
        Ok(s) => s,
        Err(_) => return Err(String::from("invalid UTF-8")),
    };
    emit_transformed(text, show_ends, show_tabs, number_nonblank, number_all, squeeze);
    Ok(())
}

fn emit_transformed(
    text: &str,
    show_ends: bool,
    show_tabs: bool,
    number_nonblank: bool,
    number_all: bool,
    squeeze: bool,
) {
    let mut lineno: usize = 1;
    let mut prev_blank = false;

    // Collect logical lines. text.lines() correctly handles trailing newline.
    // We iterate over lines() so trailing "\n" does not produce a spurious empty line.
    for line in text.lines() {
        let is_blank = line.is_empty();

        if squeeze && is_blank {
            if prev_blank {
                // suppress repeated blank
                continue;
            }
            prev_blank = true;
        } else {
            prev_blank = false;
        }

        // Number prefix
        if number_all {
            let prefix = format!("{:>6}\t", lineno);
            write_fd(1, prefix.as_bytes());
            lineno += 1;
        } else if number_nonblank && !is_blank {
            let prefix = format!("{:>6}\t", lineno);
            write_fd(1, prefix.as_bytes());
            lineno += 1;
        }

        // Apply tab transform
        if show_tabs {
            let transformed = line.replace('\t', "^I");
            write_fd(1, transformed.as_bytes());
        } else {
            write_fd(1, line.as_bytes());
        }

        // End marker + newline
        if show_ends {
            write_fd(1, b"$\n");
        } else {
            write_fd(1, b"\n");
        }
    }
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}
