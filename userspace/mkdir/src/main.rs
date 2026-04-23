//! `/bin/mkdir` — create directories, `-p` for create-with-parents.

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
    // argv[0] is the binary name ("/bin/mkdir") — skip it for flag/positional parsing.
    let operands: Vec<String> = args.into_iter().skip(1).collect();
    let (flags, positional) = parse_flags(&operands);
    if positional.is_empty() {
        let _ = debug_print("mkdir: missing operand");
        return 1;
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let _ = debug_print("mkdir: vfs unavailable");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    let mut exit_code = 0i32;
    for path in &positional {
        let resolved = libcluu::posix::resolve_path(path);
        let result = if flags.p {
            mkdir_p(&client, &resolved)
        } else {
            client.mkdir(&resolved, 0o755).map_err(|e| format!("{:?}", e))
        };
        match result {
            Ok(()) => {
                let _ = debug_print(&format!("mkdir: ok {}", resolved));
            }
            Err(err) => {
                let _ = debug_print(&format!("mkdir: {}: {}", resolved, err));
                exit_code = 1;
            }
        }
    }
    exit_code
}

struct Flags {
    p: bool,
}

fn parse_flags(args: &[String]) -> (Flags, Vec<String>) {
    let mut flags = Flags { p: false };
    let mut positional = Vec::new();
    for arg in args {
        if arg == "-p" {
            flags.p = true;
        } else if arg.starts_with('-') && arg.len() > 1 {
            let _ = debug_print(&format!("mkdir: unknown option '{}'", arg));
        } else {
            positional.push(arg.clone());
        }
    }
    (flags, positional)
}

fn mkdir_p(client: &VfsClient, path: &str) -> Result<(), String> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Ok(());
    }
    let mut current = String::new();
    for component in trimmed.split('/') {
        if component.is_empty() {
            continue;
        }
        current.push('/');
        current.push_str(component);
        match client.mkdir(&current, 0o755) {
            Ok(()) => {}
            Err(e) => {
                match client.stat(&current) {
                    Ok(info) if info.mode & 0o170000 == 0o040000 => {
                        // Already a directory — fine for -p.
                    }
                    _ => return Err(format!("{:?}", e)),
                }
            }
        }
    }
    Ok(())
}
