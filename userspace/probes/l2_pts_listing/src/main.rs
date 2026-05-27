//! Plan 2 Task 8 acceptance: a shell child can enumerate `/dev/pts` and
//! see at least the cluuterm pts that hosts its session.
//!
//! Run via `spawn l2_pts_listing` from the shell. Reports `PASS count=N`
//! when readdir returns >= 1 entry, otherwise `FAIL`.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::fs::client::VfsClient;
use libcluu::{debug_print, registry};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
        Ok(ep) => ep,
        Err(_) => {
            let _ = debug_print("ptslistprobe: FAIL vfs unavailable");
            return 1;
        }
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    match client.readdir("/dev/pts") {
        Ok(entries) => {
            let count = entries.len();
            let first_name = entries
                .first()
                .map(|e| e.name.as_str())
                .unwrap_or("<none>");
            if count >= 1 {
                let _ = debug_print(&format!(
                    "ptslistprobe: PASS count={} first={}",
                    count, first_name
                ));
                0
            } else {
                let _ = debug_print("ptslistprobe: FAIL count=0");
                1
            }
        }
        Err(err) => {
            let _ = debug_print(&format!("ptslistprobe: FAIL readdir err={:?}", err));
            1
        }
    }
}
