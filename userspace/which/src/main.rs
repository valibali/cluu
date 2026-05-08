//! /bin/which — locate a command in PATH.

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
        .program("which")
        .version("0.1.0")
        .usage("COMMAND...")
        .flag('a', "all", "print all matching pathnames of each argument")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

fn getenv_str(name: &str) -> Option<String> {
    extern "C" {
        fn getenv(name: *const u8) -> *const u8;
    }
    let mut key = String::from(name);
    key.push('\0');
    unsafe {
        let ptr = getenv(key.as_ptr());
        if ptr.is_null() {
            return None;
        }
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let bytes = core::slice::from_raw_parts(ptr, len);
        core::str::from_utf8(bytes).ok().map(String::from)
    }
}

fn vfs_stat_exists(vfs: &VfsClient, path: &str) -> bool {
    vfs.stat(path).is_ok()
}

fn find_in_path(vfs: &VfsClient, name: &str, path_env: &str, all: bool) -> Vec<String> {
    let mut results: Vec<String> = Vec::new();

    if name.contains('/') {
        if vfs_stat_exists(vfs, name) {
            results.push(String::from(name));
        }
        return results;
    }

    for dir in path_env.split(':') {
        let dir = dir.trim_end_matches('/');
        if dir.is_empty() {
            continue;
        }
        let candidate = format!("{}/{}", dir, name);
        if vfs_stat_exists(vfs, &candidate) {
            results.push(candidate);
            if !all {
                return results;
            }
        }
    }
    results
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
            write_fd(1, b"which 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("which: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.is_empty() {
        write_fd(2, b"which: missing argument\n");
        return 2;
    }

    let all = parsed.is_set("all");
    let path_env = getenv_str("PATH").unwrap_or_else(|| String::from("/bin:/usr/bin"));

    // Connect to VFS.
    let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
        Ok(e) => e,
        Err(_) => {
            write_fd(2, b"which: vfs not available\n");
            return 1;
        }
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(c) => c,
        Err(_) => {
            write_fd(2, b"which: failed to create vfs client\n");
            return 1;
        }
    };

    let mut exit_code = 0i32;
    for name in &parsed.positional {
        let matches = find_in_path(&vfs, name, &path_env, all);
        if matches.is_empty() {
            let msg = format!("which: no {} in ({})\n", name, path_env);
            write_fd(2, msg.as_bytes());
            exit_code = 1;
        } else {
            for m in &matches {
                write_fd(1, m.as_bytes());
                write_fd(1, b"\n");
            }
        }
    }
    let _ = debug_print(&format!("which: ok (exit {})", exit_code));
    exit_code
}
