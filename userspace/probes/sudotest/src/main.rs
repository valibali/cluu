//! sudotest probe: verifies sudo escalation works for admin session.
//! Lifted from SudoTestBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_IPC};
use libcluu::ipc::{call_with_payload, PROCMGR_ESCALATE_LABEL};
use libcluu::types::Message;
use libcluu::{registry, syscall};

const PROCMGR_KILL_LABEL: u32 = 3;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let procmgr_endpoint = match registry::subscribe_output("procmgr", "spawn") {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("sudotest: FAIL procmgr unavailable {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let mut payload = Vec::new();
    payload.push(0);
    payload.extend_from_slice(b"/bin/shell");
    payload.push(0);

    let notify_endpoint = match syscall::endpoint_create(process_info().tokens[TOKEN_IPC]) {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("sudotest: FAIL endpoint_create {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let mut msg = Message::new(PROCMGR_ESCALATE_LABEL, [0; 6], 2);
    msg.words[0] = payload.len();
    msg.words[1] = notify_endpoint;
    let mut reply = Message::new(0, [0; 6], 0);

    if let Err(err) = call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply) {
        let line = format!("sudotest: FAIL call {:?}", err);
        let _ = libcluu::debug_print(&line);
        return 1;
    }

    let status = reply.words[0];
    let pid = reply.words[1];
    let cid = reply.words[4];

    if status == 0 && pid != 0 {
        let line = format!("sudotest: PASS escalated pid={} cid={}", pid, cid);
        let _ = libcluu::debug_print(&line);

        // Kill the spawned shell
        let mut kill_msg = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
        kill_msg.words[0] = pid;
        kill_msg.words[1] = 9;
        let _ = libcluu::ipc::call(procmgr_endpoint, &mut kill_msg, libcluu::IpcFlags::empty());
        0
    } else {
        let line = format!("sudotest: FAIL status={} pid={}", status, pid);
        let _ = libcluu::debug_print(&line);
        1
    }
}
