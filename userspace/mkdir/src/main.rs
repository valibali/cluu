//! `/bin/mkdir` — create directories.
//!
//! Flags: -p, -v, -m MODE

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::fs::client::VfsClient;
use libcluu::posix::_write;
use libcluu::registry;

fn spec() -> Spec {
    Spec::new()
        .program("mkdir")
        .version("0.1.0")
        .usage("[-pv] [-m MODE] DIRECTORY...")
        .flag('p', "parents", "make parent directories as needed")
        .flag('v', "verbose", "print a message for each created directory")
        .required('m', "mode", "set file mode bits (octal)")
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
            write_fd(1, b"mkdir 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("mkdir: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.is_empty() {
        write_fd(2, b"mkdir: missing operand\n");
        return 1;
    }

    let parents = parsed.is_set("parents");
    let verbose = parsed.is_set("verbose");
    let mode = parsed
        .value("mode")
        .and_then(|s| usize::from_str_radix(s, 8).ok())
        .unwrap_or(0o755);

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"mkdir: vfs unavailable\n");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    let mut exit_code = 0i32;
    for path in &parsed.positional {
        let resolved = libcluu::posix::resolve_path(path);
        let result = if parents {
            mkdir_p(&client, &resolved, mode, verbose)
        } else {
            match client.mkdir(&resolved, mode) {
                Ok(()) => {
                    if verbose {
                        let msg = format!("mkdir: created directory '{}'\n", resolved);
                        write_fd(1, msg.as_bytes());
                    }
                    // legacy debug marker for harness
                    let _ = libcluu::debug_print(&format!("mkdir: ok {}", resolved));
                    Ok(())
                }
                Err(e) => Err(format!("{:?}", e)),
            }
        };
        if let Err(err) = result {
            let msg = format!("mkdir: {}: {}\n", resolved, err);
            write_fd(2, msg.as_bytes());
            // legacy debug marker
            let _ = libcluu::debug_print(&format!("mkdir: {}: {}", resolved, err));
            exit_code = 1;
        }
    }
    exit_code
}

fn mkdir_p(client: &VfsClient, path: &str, mode: usize, verbose: bool) -> Result<(), String> {
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
        match client.mkdir(&current, mode) {
            Ok(()) => {
                if verbose {
                    let msg = format!("mkdir: created directory '{}'\n", current);
                    write_fd(1, msg.as_bytes());
                }
                let _ = libcluu::debug_print(&format!("mkdir: ok {}", current));
            }
            Err(e) => match client.stat(&current) {
                Ok(info) if info.mode & 0o170000 == 0o040000 => {
                    // Already a directory — fine for -p.
                }
                _ => return Err(format!("{:?}", e)),
            },
        }
    }
    Ok(())
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}
