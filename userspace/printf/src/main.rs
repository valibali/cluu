//! /bin/printf — format and print data.

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
use libcluu::posix::_write;

fn spec() -> Spec {
    Spec::new()
        .program("printf")
        .version("0.1.0")
        .usage("FORMAT [ARG]...")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

/// Process escape sequences in a string segment (not inside a % spec).
fn process_escapes(s: &str) -> String {
    let mut out = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'n' => {
                    out.push('\n');
                    i += 2;
                }
                b't' => {
                    out.push('\t');
                    i += 2;
                }
                b'\\' => {
                    out.push('\\');
                    i += 2;
                }
                b'r' => {
                    out.push('\r');
                    i += 2;
                }
                b'0' => {
                    // \0 = NUL — skip it in output (we can't print it anyway)
                    i += 2;
                }
                _ => {
                    // Unknown escape: pass through both chars.
                    out.push('\\');
                    out.push(bytes[i + 1] as char);
                    i += 2;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
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
            write_fd(1, b"printf 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("printf: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.is_empty() {
        write_fd(2, b"printf: missing FORMAT\n");
        return 2;
    }

    let fmt = &parsed.positional[0];
    let args = &parsed.positional[1..];
    let mut arg_idx = 0usize;
    let bytes = fmt.as_bytes();
    let mut i = 0;
    let mut out = String::new();

    while i < bytes.len() {
        if bytes[i] == b'%' {
            i += 1;
            if i >= bytes.len() {
                // Trailing % — emit it.
                out.push('%');
                break;
            }
            match bytes[i] {
                b'%' => {
                    out.push('%');
                    i += 1;
                }
                b's' => {
                    let arg = args.get(arg_idx).map(|s| s.as_str()).unwrap_or("");
                    // Process escapes within %s argument too (matches GNU behaviour).
                    out.push_str(arg);
                    arg_idx += 1;
                    i += 1;
                }
                b'd' | b'i' => {
                    let arg = args.get(arg_idx).map(|s| s.as_str()).unwrap_or("0");
                    let n: i64 = arg.parse().unwrap_or(0);
                    out.push_str(&format!("{}", n));
                    arg_idx += 1;
                    i += 1;
                }
                b'x' => {
                    let arg = args.get(arg_idx).map(|s| s.as_str()).unwrap_or("0");
                    // Handle 0x prefix.
                    let stripped = arg.strip_prefix("0x").or_else(|| arg.strip_prefix("0X")).unwrap_or(arg);
                    let n: u64 = u64::from_str_radix(stripped, 16)
                        .or_else(|_| arg.parse::<u64>())
                        .unwrap_or(0);
                    out.push_str(&format!("{:x}", n));
                    arg_idx += 1;
                    i += 1;
                }
                b'X' => {
                    let arg = args.get(arg_idx).map(|s| s.as_str()).unwrap_or("0");
                    let stripped = arg.strip_prefix("0x").or_else(|| arg.strip_prefix("0X")).unwrap_or(arg);
                    let n: u64 = u64::from_str_radix(stripped, 16)
                        .or_else(|_| arg.parse::<u64>())
                        .unwrap_or(0);
                    out.push_str(&format!("{:X}", n));
                    arg_idx += 1;
                    i += 1;
                }
                b'c' => {
                    let arg = args.get(arg_idx).map(|s| s.as_str()).unwrap_or("");
                    if let Some(ch) = arg.chars().next() {
                        out.push(ch);
                    }
                    arg_idx += 1;
                    i += 1;
                }
                b'o' => {
                    let arg = args.get(arg_idx).map(|s| s.as_str()).unwrap_or("0");
                    let n: u64 = arg.parse().unwrap_or(0);
                    out.push_str(&format!("{:o}", n));
                    arg_idx += 1;
                    i += 1;
                }
                other => {
                    // Unknown specifier: emit %X literally.
                    out.push('%');
                    out.push(other as char);
                    i += 1;
                }
            }
        } else if bytes[i] == b'\\' {
            // Collect the escape sequence.
            if i + 1 < bytes.len() {
                match bytes[i + 1] {
                    b'n' => {
                        out.push('\n');
                        i += 2;
                    }
                    b't' => {
                        out.push('\t');
                        i += 2;
                    }
                    b'\\' => {
                        out.push('\\');
                        i += 2;
                    }
                    b'r' => {
                        out.push('\r');
                        i += 2;
                    }
                    b'0' => {
                        i += 2;
                    }
                    _ => {
                        out.push('\\');
                        out.push(bytes[i + 1] as char);
                        i += 2;
                    }
                }
            } else {
                out.push('\\');
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }

    // Suppress the unused variable warning.
    let _ = process_escapes;

    write_fd(1, out.as_bytes());
    let _ = debug_print("printf: ok (exit 0)");
    0
}
