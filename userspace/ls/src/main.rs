#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use libcluu::fs::client::VfsClient;
use libcluu::posix::_write;
use libcluu::registry;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    // argv[0] is "/bin/ls" or "ls"; first operand at index 1.
    // No operand → list cwd (POSIX behavior), not "/".
    let path: String = args.into_iter().nth(1)
        .unwrap_or_else(|| libcluu::posix::current_dir_string());

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let msg = b"ls: vfs not available\n";
        let _ = _write(2, msg.as_ptr() as *const _, msg.len());
        return 1;
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(c) => c,
        Err(_) => {
            let msg = b"ls: failed to create vfs client\n";
            let _ = _write(2, msg.as_ptr() as *const _, msg.len());
            return 1;
        }
    };

    let resolved = libcluu::posix::resolve_path(&path);
    match vfs.readdir(&resolved) {
        Ok(entries) => {
            for entry in entries {
                let suffix = if entry.is_dir { "/" } else { "" };
                let line = format!("{}{}\n", entry.name, suffix);
                let _ = _write(1, line.as_ptr() as *const _, line.len());
            }
            0
        }
        Err(e) => {
            let line = format!("ls: {}: {:?}\n", path, e);
            let _ = _write(2, line.as_ptr() as *const _, line.len());
            1
        }
    }
}
