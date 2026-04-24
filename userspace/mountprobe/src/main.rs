//! Harness helper for l2_mount_private: verifies that a container declared
//! with `MOUNT /tmp private` does NOT see files the caller placed in /tmp,
//! because its /tmp resolves to a fresh per-container MemFs.
//!
//! The harness shell seeds `/tmp/MOUNTPROBE_CANARY` in its own /tmp before
//! spawning us. Since the shell's /tmp is itself `private` (its own session
//! anchor) and ours is a fresh private mount, we should see an empty /tmp —
//! stat(/tmp/MOUNTPROBE_CANARY) must fail.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::fs::client::VfsClient;
use libcluu::{debug_print, registry};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
        Ok(ep) => ep,
        Err(_) => {
            let _ = debug_print("mountprobe: FAIL vfs unavailable");
            return 1;
        }
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    match client.stat("/tmp/MOUNTPROBE_CANARY") {
        Ok(_) => {
            let _ = debug_print("mountprobe: FAIL canary visible in private /tmp");
            1
        }
        Err(_) => {
            let _ = debug_print("mountprobe: PASS /tmp isolation verified");
            0
        }
    }
}
