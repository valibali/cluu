//! /bin/tr — translate or delete characters.

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
use libcluu::posix::{_read, _write};

fn spec() -> Spec {
    Spec::new()
        .program("tr")
        .version("0.1.0")
        .usage("[-ds] SET1 [SET2]")
        .flag('d', "delete", "delete characters in SET1, do not translate")
        .flag('s', "squeeze-repeats", "replace each sequence of a repeated character from SET1 with a single occurrence")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

/// Expand a tr set string, supporting `a-z` ranges and literal characters.
fn expand_set(s: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Range: X-Y where X < Y
        if i + 2 < chars.len() && chars[i + 1] == '-' {
            let lo = chars[i] as u32;
            let hi = chars[i + 2] as u32;
            if lo <= hi {
                for cp in lo..=hi {
                    if let Some(c) = char::from_u32(cp) {
                        out.push(c);
                    }
                }
                i += 3;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
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
            write_fd(1, b"tr 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("tr: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    let delete = parsed.is_set("delete");
    let squeeze = parsed.is_set("squeeze-repeats");

    if parsed.positional.is_empty() {
        write_fd(2, b"tr: missing operand\n");
        return 2;
    }

    let set1 = expand_set(&parsed.positional[0]);
    let set2 = if parsed.positional.len() > 1 {
        expand_set(&parsed.positional[1])
    } else {
        Vec::new()
    };

    // Read all stdin.
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let r = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len());
        if r <= 0 { break; }
        buf.extend_from_slice(&chunk[..r as usize]);
    }

    let input = match core::str::from_utf8(&buf) {
        Ok(s) => s,
        Err(_) => {
            write_fd(2, b"tr: input not valid UTF-8\n");
            return 1;
        }
    };

    let mut out = String::with_capacity(input.len());

    if delete {
        // Delete mode: remove chars in set1.
        let mut prev: Option<char> = None;
        for c in input.chars() {
            if set1.contains(&c) {
                // skip
            } else if squeeze && set2.contains(&c) {
                if prev != Some(c) {
                    out.push(c);
                }
                prev = Some(c);
            } else {
                out.push(c);
                prev = Some(c);
            }
        }
    } else if !set2.is_empty() {
        // Translate mode: map set1[i] → set2[i] (extend last if set2 shorter).
        let last2 = set2.last().copied().unwrap_or('\0');
        let mut prev: Option<char> = None;
        for c in input.chars() {
            let mapped = if let Some(pos) = set1.iter().position(|&x| x == c) {
                let replacement = if pos < set2.len() { set2[pos] } else { last2 };
                replacement
            } else {
                c
            };
            if squeeze && set2.contains(&mapped) {
                if prev != Some(mapped) {
                    out.push(mapped);
                }
                prev = Some(mapped);
            } else {
                out.push(mapped);
                prev = Some(mapped);
            }
        }
    } else if squeeze {
        // Squeeze only: squeeze consecutive chars in set1.
        let mut prev: Option<char> = None;
        for c in input.chars() {
            if set1.contains(&c) {
                if prev != Some(c) {
                    out.push(c);
                }
                prev = Some(c);
            } else {
                out.push(c);
                prev = Some(c);
            }
        }
    } else {
        // No-op: pass through.
        out.push_str(input);
    }

    write_fd(1, out.as_bytes());
    let _ = debug_print("tr: ok (exit 0)");
    0
}
