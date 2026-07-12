//! ownerdeny probe: verifies that non-owner cannot delete owner's file.
//! Lifted from Ext2OwnerDenyBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::boot::{process_info, TOKEN_IPC};
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{call_with_payload, recv, PROCMGR_CONTAINER_RUN_LABEL};
use libcluu::types::Message;
use libcluu::{registry, IpcFlags};

fn parse_status(raw: usize) -> libcluu::Result<()> {
    let signed = raw as isize;
    if signed < 0 {
        return Err(libcluu::Error::from_errno(signed));
    }
    Ok(())
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let path = "/tmp/l2a_owner_probe";

    let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("ext2ownerdeny: FAIL vfs unavailable {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(client) => client,
        Err(err) => {
            let line = format!("ext2ownerdeny: FAIL client {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let created = match vfs.open_with(path, 0o1000 | 2, 0o644) {
        Ok(file) => file,
        Err(err) => {
            let line = format!("ext2ownerdeny: FAIL create/open {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };
    let _ = vfs.close(created);

    // Spawn ownerprobe
    let procmgr_endpoint = match registry::subscribe_output("root-procmgr", "spawn") {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("ext2ownerdeny: FAIL procmgr unavailable {:?}", err);
            let _ = libcluu::debug_print(&line);
            let _ = vfs.unlink(path);
            return 1;
        }
    };

    let name = b"ownerprobe";
    let notify_endpoint = match libcluu::syscall::endpoint_create(process_info().tokens[TOKEN_IPC]) {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("ext2ownerdeny: FAIL endpoint_create {:?}", err);
            let _ = libcluu::debug_print(&line);
            let _ = vfs.unlink(path);
            return 1;
        }
    };

    let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3);
    msg.words[0] = name.len();
    msg.words[1] = notify_endpoint;
    msg.words[2] = 0;
    let mut reply = Message::new(0, [0; 6], 0);

    if let Err(err) = call_with_payload(procmgr_endpoint, &msg, name, &mut reply) {
        let line = format!("ext2ownerdeny: FAIL spawn call {:?}", err);
        let _ = libcluu::debug_print(&line);
        let _ = vfs.unlink(path);
        return 1;
    }

    if let Err(err) = parse_status(reply.words[0]) {
        let line = format!("ext2ownerdeny: FAIL spawn-status {:?}", err);
        let _ = libcluu::debug_print(&line);
        let _ = vfs.unlink(path);
        return 1;
    }

    // Wait for ownerprobe to exit
    let mut exit_msg = Message::new(0, [0; 6], 0);
    let _ = recv(notify_endpoint, &mut exit_msg, IpcFlags::empty());
    if exit_msg.tag.words >= 2 && exit_msg.words[1] != 0 {
        let line = format!(
            "ext2ownerdeny: FAIL ownerprobe-exit {}",
            exit_msg.words[1]
        );
        let _ = libcluu::debug_print(&line);
        let _ = vfs.unlink(path);
        return 1;
    }

    let still_exists = match vfs.stat(path) {
        Ok(_) => true,
        Err(err) => {
            let line = format!("ext2ownerdeny: FAIL stat-after {:?}", err);
            let _ = libcluu::debug_print(&line);
            false
        }
    };
    if !still_exists {
        return 1;
    }

    match vfs.unlink(path) {
        Ok(()) => {
            let _ = libcluu::debug_print(
                "ext2ownerdeny: PASS non-owner denied + owner cleanup",
            );
            0
        }
        Err(err) => {
            let line = format!("ext2ownerdeny: FAIL owner cleanup {:?}", err);
            let _ = libcluu::debug_print(&line);
            1
        }
    }
}
