//! /bin/find — search for files in directory hierarchy.

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
        .program("find")
        .version("0.1.0")
        .usage("PATH [--name PATTERN] [--type TYPE]")
        .required('N', "name", "filename pattern (glob: * and ?)")
        .required('T', "type", "f=regular, d=directory, l=symlink")
        .long_flag("print", "print matched paths (default action)")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

/// Simple glob match: only `*` and `?` wildcards.
fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    glob_match_inner(&p, &n)
}

fn glob_match_inner(p: &[char], n: &[char]) -> bool {
    if p.is_empty() {
        return n.is_empty();
    }
    if p[0] == '*' {
        // Match zero or more characters.
        for i in 0..=n.len() {
            if glob_match_inner(&p[1..], &n[i..]) {
                return true;
            }
        }
        return false;
    }
    if n.is_empty() {
        return false;
    }
    if p[0] == '?' || p[0] == n[0] {
        return glob_match_inner(&p[1..], &n[1..]);
    }
    false
}

const S_IFMT:  u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

fn type_matches(mode: u32, type_filter: &str) -> bool {
    match type_filter {
        "f" => (mode & S_IFMT) == S_IFREG,
        "d" => (mode & S_IFMT) == S_IFDIR,
        "l" => (mode & S_IFMT) == S_IFLNK,
        _   => true,
    }
}

fn basename_of(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

fn find_recursive(
    vfs: &VfsClient,
    path: &str,
    name_pattern: Option<&str>,
    type_filter: Option<&str>,
    depth: usize,
) {
    if depth > 32 { return; }

    let st = match vfs.stat(path) {
        Ok(s) => s,
        Err(_) => return,
    };

    let name = basename_of(path);

    // Check this entry.
    let name_ok = name_pattern.map_or(true, |pat| glob_match(pat, name));
    let type_ok = type_filter.map_or(true, |t| type_matches(st.mode, t));

    if name_ok && type_ok {
        let mut out = String::from(path);
        out.push('\n');
        write_fd(1, out.as_bytes());
    }

    // Recurse into directory.
    if (st.mode & S_IFMT) == S_IFDIR {
        let entries = match vfs.readdir(path) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in &entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let child = if path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", path.trim_end_matches('/'), entry.name)
            };
            find_recursive(vfs, &child, name_pattern, type_filter, depth + 1);
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // Normalize single-dash long opts before parsing.
    let raw_argv: Vec<String> = libcluu::args::args();
    let argv_norm: Vec<String> = raw_argv.iter().map(|a| match a.as_str() {
        "-name"  => String::from("--name"),
        "-type"  => String::from("--type"),
        "-print" => String::from("--print"),
        _        => a.clone(),
    }).collect();

    let sp = spec();
    let parsed = match parse(&sp, &argv_norm) {
        Ok(p) => p,
        Err(CliError::HelpRequested) => {
            write_fd(1, render_help(&sp).as_bytes());
            return 0;
        }
        Err(CliError::VersionRequested) => {
            write_fd(1, b"find 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("find: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    let name_pattern = parsed.value("name");
    let type_filter = parsed.value("type");

    // First positional is the path; default to ".".
    let start_path = if parsed.positional.is_empty() {
        String::from(".")
    } else {
        parsed.positional[0].clone()
    };

    let Ok(vfs_ep) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"find: vfs unavailable\n");
        return 1;
    };
    let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());

    let resolved = libcluu::posix::resolve_path(&start_path);
    find_recursive(&vfs, &resolved, name_pattern, type_filter, 0);

    let _ = debug_print("find: ok (exit 0)");
    0
}
