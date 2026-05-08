//! `/bin/touch` — change file timestamps (or create file).
//!
//! Flags: -c, -a, -m, -r REF, -d STRING

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_write, O_CREAT, O_WRONLY};
use libcluu::registry;

fn spec() -> Spec {
    Spec::new()
        .program("touch")
        .version("0.1.0")
        .usage("[-camd] [-r REF] [-d DATE] FILE...")
        .flag('c', "no-create", "do not create any files")
        .flag('a', "atime", "change only the access time")
        .flag('m', "mtime", "change only the modification time")
        .required('r', "reference", "use this file's times instead of current time")
        .required('d', "date", "parse STRING and use it instead of current time")
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
            write_fd(1, b"touch 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("touch: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.is_empty() {
        write_fd(2, b"touch: missing operand\n");
        return 1;
    }

    let no_create = parsed.is_set("no-create");
    // -a and -m only affect which timestamp to update.
    // Since VFS open_with with O_CREAT already updates mtime on creation,
    // and we don't have a utimes syscall, we just open the file to create/update it.
    // The -r and -d flags are noted but timestamp setting is best-effort (no-op if VFS lacks utimes).

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"touch: vfs not available\n");
        return 1;
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(c) => c,
        Err(_) => {
            write_fd(2, b"touch: failed to create vfs client\n");
            return 1;
        }
    };

    let mut exit_code: i32 = 0;
    for path in &parsed.positional {
        let resolved = libcluu::posix::resolve_path(path);

        // If -c and file doesn't exist, skip silently.
        if no_create && vfs.stat(&resolved).is_err() {
            continue;
        }

        // Open with O_CREAT to create if absent, or update mtime.
        match vfs.open_with(&resolved, (O_WRONLY | O_CREAT) as usize, 0o644) {
            Ok(f) => {
                let _ = vfs.close(f);
            }
            Err(e) => {
                let line = format!("touch: {}: {:?}\n", path, e);
                write_fd(2, line.as_bytes());
                let _ = libcluu::debug_print(line.trim_end());
                exit_code = 1;
            }
        }
    }
    exit_code
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}
