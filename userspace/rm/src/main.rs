//! `/bin/rm` — unlink files and optionally remove directory trees.
//!
//! Flags: -i, -f, -v, -d (already has -r)

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::fs::client::VfsClient;
use libcluu::posix::_write;
use libcluu::registry;

fn spec() -> Spec {
    Spec::new()
        .program("rm")
        .version("0.1.0")
        .usage("[-rRifdv] FILE...")
        .flag('r', "recursive", "remove directories and their contents recursively")
        .flag('R', "recursive-cap", "alias for -r")
        .flag('i', "interactive", "prompt before every removal (treated as no-op in batch)")
        .flag('f', "force", "ignore nonexistent files and arguments")
        .flag('d', "dir", "remove empty directories")
        .flag('v', "verbose", "explain what is being done")
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
            write_fd(1, b"rm 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("rm: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.is_empty() {
        if !parsed.is_set("force") {
            write_fd(2, b"rm: missing operand\n");
            return 1;
        }
        return 0;
    }

    // Hard guard: refuse root removal.
    for arg in &parsed.positional {
        let resolved = libcluu::posix::resolve_path(arg);
        if resolved == "/" || resolved.is_empty() {
            write_fd(2, b"rm: refusing to remove root directory\n");
            return 1;
        }
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"rm: vfs unavailable\n");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    let recursive = parsed.is_set("recursive") || parsed.is_set("recursive-cap");
    let force = parsed.is_set("force");
    let verbose = parsed.is_set("verbose");
    let remove_dir = parsed.is_set("dir");

    let flags = RmFlags { recursive, force, verbose, remove_dir };

    let mut exit_code = 0i32;
    for path in &parsed.positional {
        let resolved = libcluu::posix::resolve_path(path);
        match remove_entry(&client, &resolved, &flags) {
            Ok(()) => {
                if verbose {
                    let msg = format!("removed '{}'\n", resolved);
                    write_fd(1, msg.as_bytes());
                }
                // legacy debug marker for harness compatibility
                let _ = libcluu::debug_print(&format!("rm: ok {}", resolved));
            }
            Err(err) => {
                let msg = format!("rm: {}: {}\n", resolved, err);
                write_fd(2, msg.as_bytes());
                exit_code = 1;
            }
        }
    }
    exit_code
}

struct RmFlags {
    recursive: bool,
    force: bool,
    verbose: bool,
    remove_dir: bool,
}

fn remove_entry(client: &VfsClient, path: &str, flags: &RmFlags) -> Result<(), String> {
    let info = match client.stat(path) {
        Ok(v) => v,
        Err(e) => {
            let s = format!("{:?}", e);
            if flags.force && s.contains("NotFound") {
                return Ok(());
            }
            return Err(s);
        }
    };
    let is_dir = info.mode & 0o170000 == 0o040000;
    if is_dir {
        if flags.recursive {
            remove_tree(client, path, flags)
        } else if flags.remove_dir {
            client.rmdir(path).map_err(|e| format!("{:?}", e))
        } else {
            Err(String::from("is a directory"))
        }
    } else {
        client.unlink(path).map_err(|e| format!("{:?}", e))
    }
}

fn remove_tree(client: &VfsClient, root: &str, flags: &RmFlags) -> Result<(), String> {
    let mut pending: Vec<String> = alloc::vec![String::from(root)];
    let mut rmdir_order: Vec<String> = Vec::new();
    while let Some(dir) = pending.pop() {
        rmdir_order.push(dir.clone());
        let entries = client.readdir(&dir).map_err(|e| format!("{:?}", e))?;
        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let child = if dir.ends_with('/') {
                format!("{}{}", dir, entry.name)
            } else {
                format!("{}/{}", dir, entry.name)
            };
            if entry.is_dir {
                pending.push(child);
            } else {
                if flags.verbose {
                    let msg = format!("removed '{}'\n", child);
                    write_fd(1, msg.as_bytes());
                }
                client.unlink(&child).map_err(|e| format!("{:?}", e))?;
            }
        }
    }
    while let Some(dir) = rmdir_order.pop() {
        if flags.verbose {
            let msg = format!("removed directory '{}'\n", dir);
            write_fd(1, msg.as_bytes());
        }
        client.rmdir(&dir).map_err(|e| format!("{:?}", e))?;
    }
    Ok(())
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}
