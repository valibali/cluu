//! `/bin/cp` — copy a single file from src to dst.
//!
//! v1 scope: single src, single dst, files only (no directory copy, no -r).
//! Streams the source via zero-copy grant reads and writes via `VfsClient::write`.

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
use libcluu::posix::{O_CREAT, O_TRUNC, O_WRONLY};
use libcluu::{debug_print, registry};

const CHUNK_SIZE: usize = 64 * 1024;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    // argv[0] is "/bin/cp"; skip it.
    let operands: Vec<String> = args.into_iter().skip(1).collect();
    if operands.len() != 2 {
        let _ = debug_print("cp: usage: cp <src> <dst>");
        return 1;
    }
    let src = libcluu::posix::resolve_path(&operands[0]);
    let dst = libcluu::posix::resolve_path(&operands[1]);

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let _ = debug_print("cp: vfs unavailable");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    match copy_file(&client, &src, &dst) {
        Ok(()) => {
            let _ = debug_print(&format!("cp: ok {} -> {}", src, dst));
            0
        }
        Err(err) => {
            let _ = debug_print(&format!("cp: {}: {}", src, err));
            1
        }
    }
}

fn copy_file(client: &VfsClient, src: &str, dst: &str) -> Result<(), String> {
    // Refuse copying a directory in v1 — error early before opening dst.
    let info = client.stat(src).map_err(|e| format!("{:?}", e))?;
    let is_dir = info.mode & 0o170000 == 0o040000;
    if is_dir {
        return Err(String::from("is a directory"));
    }

    let src_file = client.open(src).map_err(|e| format!("{:?}", e))?;
    let total = src_file.size;

    let dst_file = client
        .open_with(dst, (O_WRONLY | O_CREAT | O_TRUNC) as usize, 0o644)
        .map_err(|e| {
            let _ = client.close(src_file);
            format!("dst open: {:?}", e)
        })?;

    if total == 0 {
        // Nothing to copy.
        let _ = client.close(src_file);
        let _ = client.close(dst_file);
        return Ok(());
    }

    // Allocate a single page-aligned scratch window for chunked reads.
    let info_page = process_info();
    let space_token = info_page.tokens[TOKEN_SPACE];
    let chunk_alloc = ((CHUNK_SIZE.min(total)) + 4095) & !4095;
    let scratch_base = libcluu::vspace::VSPACE
        .lock()
        .alloc(chunk_alloc)
        .map_err(|_| {
            let _ = client.close(src_file);
            let _ = client.close(dst_file);
            String::from("out of virtual memory")
        })?;

    let mut offset = 0usize;
    let mut result: Result<(), String> = Ok(());
    while offset < total {
        let remaining = total - offset;
        let want = remaining.min(CHUNK_SIZE);
        match client.read_grant(src_file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let buf =
                    unsafe { core::slice::from_raw_parts(scratch_base as *const u8, grant.len) };
                if let Err(e) = client.write(dst_file, offset, buf) {
                    result = Err(format!("write: {:?}", e));
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
    let _ = client.close(src_file);
    let _ = client.close(dst_file);
    result
}
