//! `/bin/cat` — concatenate files and print to stdout.
//!
//! With path operands: opens each file via VFS and streams it to fd 1.
//! With no operands: copies fd 0 (stdin) to fd 1 until EOF.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_read, _write};
use libcluu::registry;

const CHUNK_SIZE: usize = 64 * 1024;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    // argv[0] is "/bin/cat" or "cat"; operands start at index 1.
    let operands: Vec<String> = args.into_iter().skip(1).collect();

    if operands.is_empty() {
        // No paths: copy fd 0 to fd 1 until EOF.
        return cat_stdin();
    }

    // Open the VFS once and reuse for all paths.
    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_err(b"cat: vfs unavailable\n");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let vfs = VfsClient::new(vfs_endpoint, client_id);

    let mut exit_code: i32 = 0;
    for path in operands {
        let resolved = libcluu::posix::resolve_path(&path);
        if let Err(reason) = cat_file(&vfs, &resolved) {
            let line = format!("cat: {}: {}\n", path, reason);
            write_err(line.as_bytes());
            exit_code = 1;
        }
    }
    exit_code
}

fn cat_stdin() -> i32 {
    let mut buf = [0u8; CHUNK_SIZE];
    loop {
        let n = _read(0, buf.as_mut_ptr() as *mut _, buf.len());
        if n == 0 {
            return 0; // EOF
        }
        if n < 0 {
            return 1; // error
        }
        let m = _write(1, buf.as_ptr() as *const _, n as usize);
        if m < 0 {
            return 1;
        }
    }
}

fn cat_file(vfs: &VfsClient, path: &str) -> Result<(), String> {
    let file = vfs.open(path).map_err(|e| format!("{:?}", e))?;
    let total = file.size;
    if total == 0 {
        let _ = vfs.close(file);
        return Ok(());
    }

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let chunk_alloc = ((CHUNK_SIZE.min(total)) + 4095) & !4095;
    let scratch_base = libcluu::vspace::VSPACE
        .lock()
        .alloc(chunk_alloc)
        .map_err(|_| {
            let _ = vfs.close(file);
            String::from("out of virtual memory")
        })?;

    let mut offset = 0usize;
    let mut result: Result<(), String> = Ok(());
    while offset < total {
        let remaining = total - offset;
        let want = remaining.min(CHUNK_SIZE);
        match vfs.read_grant(file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let slice = unsafe {
                    core::slice::from_raw_parts(scratch_base as *const u8, grant.len)
                };
                let m = _write(1, slice.as_ptr() as *const _, slice.len());
                if m < 0 {
                    result = Err(String::from("write failed"));
                    break;
                }
                offset += grant.len;
            }
            Err(e) => {
                result = Err(format!("read: {:?}", e));
                break;
            }
        }
    }

    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, chunk_alloc);
    let _ = vfs.close(file);
    result
}

fn write_err(line: &[u8]) {
    let _ = _write(2, line.as_ptr() as *const _, line.len());
}
