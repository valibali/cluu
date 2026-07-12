//! `/bin/sed` — stream editor (substitute command only).
//!
//! Supports: `s/pattern/replacement/flags`
//! Patterns: literal, `.` wildcard, `*` repetition
//! Flags: g (global), p (print), i (case-insensitive)

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_read, _write};
use libcluu::registry;

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

fn char_eq(a: u8, b: u8, ci: bool) -> bool {
    if ci {
        a.to_ascii_lowercase() == b.to_ascii_lowercase()
    } else {
        a == b
    }
}

/// Match `pattern` at the start of `text`. Returns match length on success.
fn match_here(pat: &[u8], text: &[u8], ci: bool) -> Option<usize> {
    if pat.is_empty() {
        return Some(0);
    }
    if pat.len() >= 2 && pat[1] == b'*' {
        return match_star(pat[0], &pat[2..], text, ci);
    }
    if !text.is_empty() && (pat[0] == b'.' || char_eq(pat[0], text[0], ci)) {
        match_here(&pat[1..], &text[1..], ci).map(|r| r + 1)
    } else {
        None
    }
}

fn match_star(c: u8, rest: &[u8], text: &[u8], ci: bool) -> Option<usize> {
    let mut max = 0;
    while max < text.len() && (c == b'.' || char_eq(c, text[max], ci)) {
        max += 1;
    }
    loop {
        if let Some(r) = match_here(rest, &text[max..], ci) {
            return Some(max + r);
        }
        if max == 0 {
            break;
        }
        max -= 1;
    }
    None
}

/// Find first match of `pat` in `text`. Returns (start, length).
fn find_match(pat: &[u8], text: &[u8], ci: bool) -> Option<(usize, usize)> {
    for start in 0..=text.len() {
        if let Some(len) = match_here(pat, &text[start..], ci) {
            if len > 0 || start == text.len() {
                return Some((start, len));
            }
        }
    }
    None
}

struct SubstCmd {
    pattern: Vec<u8>,
    replacement: Vec<u8>,
    global: bool,
    print: bool,
    case_insensitive: bool,
}

/// Parse `s/pat/repl/flags` into a SubstCmd.
fn parse_subst(cmd: &str) -> Option<SubstCmd> {
    let bytes = cmd.as_bytes();
    if bytes.is_empty() || bytes[0] != b's' {
        return None;
    }
    let delim = bytes.get(1).copied()?;
    let rest = &bytes[2..];
    let mut parts: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == delim {
            parts.push(cur.clone());
            cur.clear();
        } else {
            cur.push(rest[i]);
        }
        i += 1;
    }
    parts.push(cur);
    if parts.len() < 3 {
        return None;
    }
    let flags = &parts[2];
    let global = flags.contains(&b'g');
    let print = flags.contains(&b'p');
    let case_insensitive = flags.contains(&b'i');
    Some(SubstCmd {
        pattern: parts[0].clone(),
        replacement: parts[1].clone(),
        global,
        print,
        case_insensitive,
    })
}

fn substitute_line(cmd: &SubstCmd, line: &str) -> (String, bool) {
    let text = line.as_bytes();
    if cmd.pattern.is_empty() {
        return (String::from(line), false);
    }
    let mut result: Vec<u8> = Vec::new();
    let mut pos = 0;
    let mut changed = false;
    while pos <= text.len() {
        let slice = &text[pos..];
        match find_match(&cmd.pattern, slice, cmd.case_insensitive) {
            Some((start, len)) => {
                result.extend_from_slice(&slice[..start]);
                result.extend_from_slice(&cmd.replacement);
                pos += start + len;
                changed = true;
                if len == 0 {
                    if pos < text.len() {
                        result.push(text[pos]);
                        pos += 1;
                    }
                }
                if !cmd.global {
                    result.extend_from_slice(&text[pos..]);
                    break;
                }
            }
            None => {
                result.extend_from_slice(&text[pos..]);
                break;
            }
        }
    }
    let out = String::from_utf8_lossy(&result).into_owned();
    (out, changed)
}

fn read_whole_file_into(vfs: &VfsClient, path: &str, dst: &mut Vec<u8>) -> Result<(), ()> {
    use libcluu::boot::{process_info, TOKEN_SPACE};
    const FCHUNK: usize = 64 * 1024;
    let file = vfs.open(path).map_err(|_| ())?;
    let total = file.size;
    if total == 0 {
        let _ = vfs.close(file);
        return Ok(());
    }
    let info = process_info();
    let tok = info.tokens[TOKEN_SPACE];
    let sz = ((FCHUNK.min(total)) + 4095) & !4095;
    let base = libcluu::vspace::VSPACE
        .lock()
        .alloc(sz)
        .map_err(|_| { let _ = vfs.close(file); })?;
    let mut off = 0;
    let mut res: Result<(), ()> = Ok(());
    while off < total {
        let want = (total - off).min(FCHUNK);
        match vfs.read_grant(file, off, want, tok, base) {
            Ok(g) => {
                if g.len == 0 { break; }
                let s = unsafe { core::slice::from_raw_parts(base as *const u8, g.len) };
                dst.extend_from_slice(s);
                off += g.len;
            }
            Err(_) => { res = Err(()); break; }
        }
    }
    let _ = libcluu::vspace::VSPACE.lock().free(base, sz);
    let _ = vfs.close(file);
    res
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("SED_OK\n");
    let argv: Vec<String> = libcluu::args::args();
    if argv.len() < 2 {
        write_fd(2, b"sed: usage: sed 's/pat/repl/flags' [FILE]\n");
        return 1;
    }
    let cmd = match parse_subst(&argv[1]) {
        Some(c) => c,
        None => {
            write_fd(2, b"sed: invalid command\n");
            return 1;
        }
    };

    let text: String = if let Some(path) = argv.get(2) {
        let Ok(vfs_ep) = registry::subscribe_output("vfs", "main") else {
            write_fd(2, b"sed: vfs unavailable\n");
            return 1;
        };
        let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());
        let resolved = libcluu::posix::resolve_path(path);
        let mut buf: Vec<u8> = Vec::new();
        if read_whole_file_into(&vfs, &resolved, &mut buf).is_err() {
            let m = format!("sed: {}: cannot read\n", path);
            write_fd(2, m.as_bytes());
            return 1;
        }
        match String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => { write_fd(2, b"sed: not valid UTF-8\n"); return 1; }
        }
    } else {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let r = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len());
            if r <= 0 { break; }
            buf.extend_from_slice(&chunk[..r as usize]);
        }
        match String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => { write_fd(2, b"sed: not valid UTF-8\n"); return 1; }
        }
    };

    for line in text.lines() {
        let (out, changed) = substitute_line(&cmd, line);
        write_fd(1, out.as_bytes());
        write_fd(1, b"\n");
        if cmd.print && changed {
            write_fd(1, out.as_bytes());
            write_fd(1, b"\n");
        }
    }

    let _ = debug_print("sed: ok (exit 0)");
    0
}
