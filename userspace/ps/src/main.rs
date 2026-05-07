#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::posix::_write;
use libcluu::registry;

const GRANT_SIZE: usize = 4096;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let msg = b"ps: vfs not available\n";
        let _ = _write(2, msg.as_ptr() as *const _, msg.len());
        return 1;
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(c) => c,
        Err(_) => {
            let msg = b"ps: failed to connect to vfs\n";
            let _ = _write(2, msg.as_ptr() as *const _, msg.len());
            return 1;
        }
    };

    let header = b"  PID  NAME             STATE  TICKS     MEM\n";
    let _ = _write(1, header.as_ptr() as *const _, header.len());

    let entries = match vfs.readdir("/proc") {
        Ok(e) => e,
        Err(_) => {
            let msg = b"ps: failed to read /proc\n";
            let _ = _write(2, msg.as_ptr() as *const _, msg.len());
            return 1;
        }
    };

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let grant_base = match libcluu::vspace::VSPACE.lock().alloc(GRANT_SIZE) {
        Ok(addr) => addr,
        Err(_) => {
            let msg = b"ps: out of virtual memory\n";
            let _ = _write(2, msg.as_ptr() as *const _, msg.len());
            return 1;
        }
    };

    for entry in &entries {
        if !entry.is_dir
            || entry.name.is_empty()
            || !entry.name.bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }

        let stat_path = format!("/proc/{}/stat", entry.name);
        let file = match vfs.open(&stat_path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        if file.size == 0 {
            let _ = vfs.close(file);
            continue;
        }

        let read_size = file.size.min(GRANT_SIZE);
        if let Ok(grant) = vfs.read_grant(file, 0, read_size, space_token, grant_base) {
            if grant.len > 0 && grant.offset + grant.len <= GRANT_SIZE {
                let addr = grant.base + grant.offset;
                let data = unsafe {
                    core::slice::from_raw_parts(addr as *const u8, grant.len)
                };
                let text = core::str::from_utf8(data).unwrap_or("");

                let parts: Vec<&str> = text.trim().splitn(6, ' ').collect();
                if parts.len() >= 6 {
                    let pid = parts[0];
                    let name = parts[1];
                    let state = parts[2];
                    let ticks = parts[3];
                    let heap = parts[4].parse::<u32>().unwrap_or(0);
                    let other = parts[5].trim().parse::<u32>().unwrap_or(0);
                    let mem_kb = (heap as u64 + other as u64) * 4;
                    let line = format!(
                        "{:>5}  {:<16} {:>5}  {:>8}  {:>4}K\n",
                        pid, name, state, ticks, mem_kb
                    );
                    let _ = _write(1, line.as_ptr() as *const _, line.len());
                }
            }
        }

        let _ = vfs.close(file);
    }

    let _ = libcluu::vspace::VSPACE.lock().free(grant_base, GRANT_SIZE);
    0
}
