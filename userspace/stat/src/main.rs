//! /bin/stat — display file status.

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
use libcluu::posix::_write;
use libcluu::registry;

fn spec() -> Spec {
    Spec::new()
        .program("stat")
        .version("0.1.0")
        .usage("[-c FORMAT] FILE...")
        .required('c', "format", "use the specified FORMAT instead of the default")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

// Mode bit constants.
const S_IFMT:  u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const S_IFCHR: u32 = 0o020000;
const S_IFBLK: u32 = 0o060000;
const S_IFIFO: u32 = 0o010000;
const S_IFSOCK:u32 = 0o140000;

fn render_mode_symbolic(mode: u32) -> String {
    let mut s = String::with_capacity(10);
    s.push(match mode & S_IFMT {
        S_IFDIR  => 'd',
        S_IFREG  => '-',
        S_IFLNK  => 'l',
        S_IFCHR  => 'c',
        S_IFBLK  => 'b',
        S_IFIFO  => 'p',
        S_IFSOCK => 's',
        _        => '?',
    });
    let bits: &[(u32, char)] = &[
        (0o400, 'r'), (0o200, 'w'), (0o100, 'x'),
        (0o040, 'r'), (0o020, 'w'), (0o010, 'x'),
        (0o004, 'r'), (0o002, 'w'), (0o001, 'x'),
    ];
    for &(mask, ch) in bits {
        s.push(if mode & mask != 0 { ch } else { '-' });
    }
    s
}

fn unix_to_ymdhms(t: u64) -> (u32, u32, u32, u32, u32, u32) {
    let secs_in_day = 86_400u64;
    let days = t / secs_in_day;
    let rem  = t % secs_in_day;
    let hh = (rem / 3600) as u32;
    let mm = ((rem % 3600) / 60) as u32;
    let ss = (rem % 60) as u32;

    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe/1460 + doe/36524 - doe/146_096) / 365;
    let y_off = yoe as i64 + era * 400;
    let doy = doe - (365*yoe + yoe/4 - yoe/100);
    let mp = (5*doy + 2) / 153;
    let d = (doy - (153*mp + 2)/5 + 1) as u32;
    let m = if mp < 10 { mp as u32 + 3 } else { mp as u32 - 9 };
    let y = (if m <= 2 { y_off + 1 } else { y_off }) as u32;
    (y, m, d, hh, mm, ss)
}

fn basename_of(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

fn stat_default(path: &str, vfs: &VfsClient) -> i32 {
    let resolved = libcluu::posix::resolve_path(path);
    let st = match vfs.stat(&resolved) {
        Ok(s) => s,
        Err(_) => {
            let msg = format!("stat: cannot statx '{}': No such file or directory\n", path);
            write_fd(2, msg.as_bytes());
            return 1;
        }
    };

    let name = basename_of(path);
    let sym = render_mode_symbolic(st.mode);
    let perms_octal = st.mode & 0o7777;
    let (y, mo, d, hh, mm, ss) = unix_to_ymdhms(st.mtime);

    let out = format!(
        "  File: {}\n  Size: {:<15} Blocks: {:<10} IO Block: 4096\nAccess: ({:04o}/{})  Uid: ({})  Gid: ({})\nModify: {:04}-{:02}-{:02} {:02}:{:02}:{:02}\n",
        name,
        st.size,
        st.blocks,
        perms_octal,
        sym,
        st.uid,
        st.gid,
        y, mo, d, hh, mm, ss,
    );
    write_fd(1, out.as_bytes());
    0
}

fn stat_format(path: &str, vfs: &VfsClient, fmt: &str) -> i32 {
    let resolved = libcluu::posix::resolve_path(path);
    let st = match vfs.stat(&resolved) {
        Ok(s) => s,
        Err(_) => {
            let msg = format!("stat: cannot statx '{}': No such file or directory\n", path);
            write_fd(2, msg.as_bytes());
            return 1;
        }
    };

    let name = basename_of(path);
    let mut out = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && i + 1 < chars.len() {
            i += 1;
            match chars[i] {
                'n' => out.push_str(name),
                's' => out.push_str(&format!("{}", st.size)),
                'a' => out.push_str(&format!("{:o}", st.mode & 0o7777)),
                'U' => out.push_str(&format!("{}", st.uid)),
                'G' => out.push_str(&format!("{}", st.gid)),
                'Y' => out.push_str(&format!("{}", st.mtime)),
                other => {
                    out.push('%');
                    out.push(other);
                }
            }
        } else {
            out.push(chars[i]);
        }
        i += 1;
    }
    out.push('\n');
    write_fd(1, out.as_bytes());
    0
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
            write_fd(1, b"stat 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("stat: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.is_empty() {
        write_fd(2, b"stat: missing operand\n");
        return 2;
    }

    let fmt = parsed.value("format").map(String::from);

    let Ok(vfs_ep) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"stat: vfs unavailable\n");
        return 1;
    };
    let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());

    let mut exit_code = 0i32;
    for path in &parsed.positional {
        let ec = if let Some(ref f) = fmt {
            stat_format(path, &vfs, f)
        } else {
            stat_default(path, &vfs)
        };
        if ec != 0 { exit_code = ec; }
    }

    let _ = debug_print(&format!("stat: ok (exit {})", exit_code));
    exit_code
}
