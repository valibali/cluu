#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::vec::Vec;
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_write, O_CREAT, O_WRONLY};
use libcluu::{debug_print, registry};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let operands: Vec<_> = args.into_iter().skip(1).collect();
    if operands.is_empty() {
        let msg = b"touch: missing operand\n";
        let _ = _write(2, msg.as_ptr() as *const _, msg.len());
        return 1;
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let msg = b"touch: vfs not available\n";
        let _ = _write(2, msg.as_ptr() as *const _, msg.len());
        return 1;
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(c) => c,
        Err(_) => {
            let msg = b"touch: failed to create vfs client\n";
            let _ = _write(2, msg.as_ptr() as *const _, msg.len());
            return 1;
        }
    };

    let mut exit_code: i32 = 0;
    for path in operands {
        let resolved = libcluu::posix::resolve_path(&path);
        match vfs.open_with(
            &resolved,
            (O_WRONLY | O_CREAT) as usize,
            0o644,
        ) {
            Ok(f) => {
                let _ = vfs.close(f);
            }
            Err(e) => {
                let line = format!("touch: {}: {:?}\n", path, e);
                let _ = _write(2, line.as_ptr() as *const _, line.len());
                // Mirror the failure to the kernel debug stream so the
                // harness (which scrapes COM2) can observe it; tty/console
                // output never reaches serial.
                let _ = debug_print(line.trim_end());
                exit_code = 1;
            }
        }
    }
    exit_code
}
