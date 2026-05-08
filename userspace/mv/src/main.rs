//! `/bin/mv` — rename or move files.
//!
//! Flags: -i, -f, -n, -v

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::fs::client::VfsClient;
use libcluu::posix::_write;
use libcluu::registry;

fn spec() -> Spec {
    Spec::new()
        .program("mv")
        .version("0.1.0")
        .usage("[-ifnv] SOURCE DEST")
        .flag('i', "interactive", "prompt before overwrite (treated as -n)")
        .flag('f', "force", "do not prompt before overwriting")
        .flag('n', "no-clobber", "do not overwrite an existing file")
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
            write_fd(1, b"mv 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("mv: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.len() != 2 {
        write_fd(2, b"mv: usage: mv [-ifnv] SOURCE DEST\n");
        return 2;
    }

    let no_clobber = parsed.is_set("no-clobber") || parsed.is_set("interactive");
    let verbose = parsed.is_set("verbose");

    let src = libcluu::posix::resolve_path(&parsed.positional[0]);
    let dst = libcluu::posix::resolve_path(&parsed.positional[1]);

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"mv: vfs unavailable\n");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    // No-clobber: refuse if dst exists.
    if no_clobber && client.stat(&dst).is_ok() {
        return 0;
    }

    match client.rename(&src, &dst) {
        Ok(()) => {
            if verbose {
                let msg = format!("'{}' -> '{}'\n", src, dst);
                write_fd(1, msg.as_bytes());
            }
            0
        }
        Err(e) => {
            let msg = format!("mv: {} -> {}: {:?}\n", src, dst, e);
            write_fd(2, msg.as_bytes());
            1
        }
    }
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}
