//! `/bin/mv` — rename or move a single file/directory from src to dst.
//!
//! v1 scope: single src, single dst. Uses `VfsClient::rename` which is
//! atomic on the VFS side. Cross-filesystem moves are not implemented
//! (rename is in-mount-table only); a future revision can fall back to
//! copy+unlink for cross-mount renames.

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
    let operands: Vec<String> = args.into_iter().skip(1).collect();
    if operands.len() != 2 {
        let _ = debug_print("mv: usage: mv <src> <dst>");
        return 1;
    }
    let src = libcluu::posix::resolve_path(&operands[0]);
    let dst = libcluu::posix::resolve_path(&operands[1]);

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let _ = debug_print("mv: vfs unavailable");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    match client.rename(&src, &dst) {
        Ok(()) => {
            let _ = debug_print(&format!("mv: ok {} -> {}", src, dst));
            0
        }
        Err(e) => {
            let _ = debug_print(&format!("mv: {} -> {}: {:?}", src, dst, e));
            1
        }
    }
}
