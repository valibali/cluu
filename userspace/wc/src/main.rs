//! `/bin/wc` — count lines, words, bytes, chars, and max line length.
//!
//! Flags: -l, -w, -c, -m, -L; default = -lwc; multi-file totals row.

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
        .program("wc")
        .version("0.1.0")
        .usage("[-lwcmL] [FILE]...")
        .flag('l', "lines", "print the newline counts")
        .flag('w', "words", "print the word counts")
        .flag('c', "bytes", "print the byte counts")
        .flag('m', "chars", "print the character counts")
        .flag('L', "max-line-length", "print the maximum display width")
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
            write_fd(1, b"wc 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("wc: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    let want_l = parsed.is_set("lines");
    let want_w = parsed.is_set("words");
    let want_c = parsed.is_set("bytes");
    let want_m = parsed.is_set("chars");
    #[allow(non_snake_case)]
    let want_L = parsed.is_set("max-line-length");

    // Default = -lwc if no flags given.
    let any_flag = want_l || want_w || want_c || want_m || want_L;
    #[allow(non_snake_case)]
    let (show_l, show_w, show_c, show_m, show_L) = if any_flag {
        (want_l, want_w, want_c, want_m, want_L)
    } else {
        (true, true, true, false, false)
    };

    if parsed.positional.is_empty() {
        // Read from stdin.
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let r = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len());
            if r <= 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..r as usize]);
        }
        let counts = count(&buf);
        print_counts(&counts, None, show_l, show_w, show_c, show_m, show_L);
        return 0;
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"wc: vfs unavailable\n");
        return 1;
    };
    let vfs = VfsClient::new(vfs_endpoint, registry::control_endpoint());

    let multi = parsed.positional.len() > 1;
    let mut totals = Counts::default();
    let mut exit_code = 0i32;

    for path in &parsed.positional {
        let mut buf: Vec<u8> = Vec::new();
        let resolved = libcluu::posix::resolve_path(path);
        if read_whole_file_into(&vfs, &resolved, &mut buf).is_err() {
            let msg = format!("wc: {}: cannot read\n", path);
            write_fd(2, msg.as_bytes());
            exit_code = 1;
            continue;
        }
        let counts = count(&buf);
        totals.lines += counts.lines;
        totals.words += counts.words;
        totals.bytes += counts.bytes;
        totals.chars += counts.chars;
        if counts.max_line > totals.max_line {
            totals.max_line = counts.max_line;
        }
        print_counts(&counts, Some(path.as_str()), show_l, show_w, show_c, show_m, show_L);
    }

    if multi {
        print_counts(&totals, Some("total"), show_l, show_w, show_c, show_m, show_L);
    }

    let _ = debug_print(&format!("wc: ok (exit {})", exit_code));
    exit_code
}

#[derive(Default)]
struct Counts {
    lines: usize,
    words: usize,
    bytes: usize,
    chars: usize,
    max_line: usize,
}

fn count(buf: &[u8]) -> Counts {
    let lines = buf.iter().filter(|&&b| b == b'\n').count();
    let words = match core::str::from_utf8(buf) {
        Ok(s) => s.split_whitespace().count(),
        Err(_) => 0,
    };
    let bytes = buf.len();
    let chars = match core::str::from_utf8(buf) {
        Ok(s) => s.chars().count(),
        Err(_) => bytes,
    };
    let max_line = match core::str::from_utf8(buf) {
        Ok(s) => s.lines().map(|l| l.len()).max().unwrap_or(0),
        Err(_) => 0,
    };
    Counts { lines, words, bytes, chars, max_line }
}

fn print_counts(
    c: &Counts,
    path: Option<&str>,
    show_l: bool,
    show_w: bool,
    show_c: bool,
    show_m: bool,
    show_max: bool,
) {
    let mut out = String::new();
    if show_l {
        out.push_str(&format!(" {:>7}", c.lines));
    }
    if show_w {
        out.push_str(&format!(" {:>7}", c.words));
    }
    if show_c {
        out.push_str(&format!(" {:>7}", c.bytes));
    }
    if show_m {
        out.push_str(&format!(" {:>7}", c.chars));
    }
    if show_max {
        out.push_str(&format!(" {:>7}", c.max_line));
    }
    if let Some(p) = path {
        out.push(' ');
        out.push_str(p);
    }
    out.push('\n');
    write_fd(1, out.as_bytes());
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
