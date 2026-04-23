//! `/bin/rm` — unlink files and optionally remove directory trees.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::fs::client::VfsClient;
use libcluu::{debug_print, registry};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    // argv[0] is "/bin/rm" (procmgr prepends the binary path); skip it for flag/positional parsing.
    let operands: Vec<String> = args.into_iter().skip(1).collect();
    let (flags, positional) = parse_flags(&operands);
    if positional.is_empty() {
        let _ = debug_print("rm: missing operand");
        return 1;
    }

    // Hard guard: refuse root removal before any processing.
    for arg in &positional {
        let resolved = libcluu::posix::resolve_path(arg);
        if resolved == "/" || resolved.is_empty() {
            let _ = debug_print("rm: refusing to remove root directory");
            return 1;
        }
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let _ = debug_print("rm: vfs unavailable");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    let mut exit_code = 0i32;
    for path in &positional {
        let resolved = libcluu::posix::resolve_path(path);
        match remove_entry(&client, &resolved, &flags) {
            Ok(()) => {
                let _ = debug_print(&format!("rm: ok {}", resolved));
            }
            Err(err) => {
                let _ = debug_print(&format!("rm: {}: {}", resolved, err));
                exit_code = 1;
            }
        }
    }
    exit_code
}

struct Flags {
    r: bool,
    f: bool,
}

fn parse_flags(args: &[String]) -> (Flags, Vec<String>) {
    let mut flags = Flags { r: false, f: false };
    let mut positional = Vec::new();
    for arg in args {
        if let Some(rest) = arg.strip_prefix('-') {
            if rest.is_empty() {
                positional.push(arg.clone());
                continue;
            }
            for ch in rest.chars() {
                match ch {
                    'r' | 'R' => flags.r = true,
                    'f' => flags.f = true,
                    other => {
                        let _ = debug_print(&format!("rm: unknown option '-{}'", other));
                    }
                }
            }
        } else {
            positional.push(arg.clone());
        }
    }
    (flags, positional)
}

fn remove_entry(client: &VfsClient, path: &str, flags: &Flags) -> Result<(), String> {
    let info = match client.stat(path) {
        Ok(v) => v,
        Err(e) => {
            // `-f` suppresses ENOENT; other errors still surface.
            let s = format!("{:?}", e);
            if flags.f && s.contains("NotFound") {
                return Ok(());
            }
            return Err(s);
        }
    };
    let is_dir = info.mode & 0o170000 == 0o040000;
    if is_dir {
        if !flags.r {
            return Err(String::from("is a directory"));
        }
        remove_tree(client, path)
    } else {
        client.unlink(path).map_err(|e| format!("{:?}", e))
    }
}

fn remove_tree(client: &VfsClient, root: &str) -> Result<(), String> {
    // Post-order iterative removal. Work stack holds dirs pending rmdir.
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
                client.unlink(&child).map_err(|e| format!("{:?}", e))?;
            }
        }
    }
    // rmdir in reverse discovery order (children before parents).
    while let Some(dir) = rmdir_order.pop() {
        client.rmdir(&dir).map_err(|e| format!("{:?}", e))?;
    }
    Ok(())
}
