//! /bin/du — estimate file space usage.

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
        .program("du")
        .version("0.1.0")
        .usage("[-sh] PATH...")
        .flag('s', "summarize", "display only a total for each argument")
        .flag('h', "human-readable", "print sizes in human readable format")
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

fn human_size(blocks: u64) -> String {
    // blocks are 512-byte units; convert to 1K units for display.
    let kb = (blocks + 1) / 2;
    if kb < 1024 {
        return format!("{}K", kb);
    }
    let mb = kb / 1024;
    if mb < 1024 {
        return format!("{}M", mb);
    }
    let gb = mb / 1024;
    format!("{}G", gb)
}

fn print_entry(blocks: u64, path: &str, human: bool) {
    let size_str = if human {
        human_size(blocks)
    } else {
        // Print in 512-byte blocks like POSIX du, rounded up to 1k-block units.
        let kb = (blocks + 1) / 2;
        format!("{}", kb)
    };
    let out = format!("{}\t{}\n", size_str, path);
    write_fd(1, out.as_bytes());
}

/// Recursively sum blocks for a path; returns total 512-byte block count.
fn du_recursive(vfs: &VfsClient, path: &str, summarize: bool, human: bool) -> u64 {
    let st = match vfs.stat(path) {
        Ok(s) => s,
        Err(_) => {
            let msg = format!("du: cannot access '{}'\n", path);
            write_fd(2, msg.as_bytes());
            return 0;
        }
    };

    let is_dir = (st.mode & 0o170000) == 0o040000;

    if !is_dir {
        if !summarize {
            print_entry(st.blocks, path, human);
        }
        return st.blocks;
    }

    // It's a directory: recurse.
    let entries = match vfs.readdir(path) {
        Ok(e) => e,
        Err(_) => {
            if !summarize {
                print_entry(st.blocks, path, human);
            }
            return st.blocks;
        }
    };

    let mut total = st.blocks;
    for entry in &entries {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        let child = if path == "/" {
            format!("/{}", entry.name)
        } else {
            format!("{}/{}", path.trim_end_matches('/'), entry.name)
        };
        let child_blocks = du_recursive(vfs, &child, summarize, human);
        total += child_blocks;
    }

    if !summarize {
        print_entry(total, path, human);
    }
    total
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
            write_fd(1, b"du 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("du: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    let summarize = parsed.is_set("summarize");
    let human = parsed.is_set("human-readable");

    let paths: Vec<String> = if parsed.positional.is_empty() {
        let cwd = libcluu::posix::current_dir_string();
        alloc::vec![cwd]
    } else {
        parsed.positional.clone()
    };

    let Ok(vfs_ep) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"du: vfs unavailable\n");
        return 1;
    };
    let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());

    for path in &paths {
        let resolved = libcluu::posix::resolve_path(path);
        let total = du_recursive(&vfs, &resolved, summarize, human);
        if summarize {
            print_entry(total, path, human);
        }
    }

    let _ = debug_print("du: ok (exit 0)");
    0
}
