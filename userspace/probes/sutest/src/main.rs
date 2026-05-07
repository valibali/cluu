//! sutest probe: verifies su session switching and equal-profile rejection.
//!
//! Subcmds (argv[1]):
//!   default  — attempt su to "alice", expect success
//!   equal    — attempt su to "root" (same ceiling), expect rejection
//!
//! Lifted from SuTestBuiltin and SuEqualTestBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_IPC};
use libcluu::ipc::{call_with_payload, PROCMGR_SU_LABEL};
use libcluu::types::Message;
use libcluu::{registry, syscall};

const PROCMGR_KILL_LABEL: u32 = 3;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let subcmd = args.get(1).map_or("default", |s| s.as_str());

    match subcmd {
        "equal" => run_equal(),
        _ => run_default(),
    }
}

fn run_default() -> i32 {
    let target = "alice";

    let procmgr_endpoint = match registry::subscribe_output("procmgr", "spawn") {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("sutest: FAIL procmgr unavailable {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let mut payload = Vec::new();
    payload.extend_from_slice(target.as_bytes());
    payload.push(0);
    payload.push(0);

    let notify_endpoint = match syscall::endpoint_create(process_info().tokens[TOKEN_IPC]) {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("sutest: FAIL endpoint_create {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let mut msg = Message::new(PROCMGR_SU_LABEL, [0; 6], 2);
    msg.words[0] = payload.len();
    msg.words[1] = notify_endpoint;
    let mut reply = Message::new(0, [0; 6], 0);

    if let Err(err) = call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply) {
        let line = format!("sutest: FAIL call {:?}", err);
        let _ = libcluu::debug_print(&line);
        return 1;
    }

    let status = reply.words[0];
    let pid = reply.words[1];
    let cid = reply.words[4];

    if status == 0 && pid != 0 {
        let line = format!(
            "sutest: PASS nested session user={} pid={} cid={}",
            target, pid, cid
        );
        let _ = libcluu::debug_print(&line);

        let mut kill_msg = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
        kill_msg.words[0] = pid;
        kill_msg.words[1] = 9;
        let _ = libcluu::ipc::call(procmgr_endpoint, &mut kill_msg, libcluu::IpcFlags::empty());
        0
    } else {
        let line = format!("sutest: FAIL status={} pid={}", status, pid);
        let _ = libcluu::debug_print(&line);
        1
    }
}

fn run_equal() -> i32 {
    let target = "root";

    let procmgr_endpoint = match registry::subscribe_output("procmgr", "spawn") {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("suequaltest: FAIL procmgr unavailable {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let mut payload = Vec::new();
    payload.extend_from_slice(target.as_bytes());
    payload.push(0);
    payload.push(0);

    let mut msg = Message::new(PROCMGR_SU_LABEL, [0; 6], 2);
    msg.words[0] = payload.len();
    msg.words[1] = 0; // no notify
    let mut reply = Message::new(0, [0; 6], 0);

    if let Err(err) = call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply) {
        let line = format!("suequaltest: FAIL call {:?}", err);
        let _ = libcluu::debug_print(&line);
        return 1;
    }

    let status = reply.words[0];
    if status != 0 {
        let line = format!(
            "suequaltest: PASS su equal-profile rejected (errno={})",
            status
        );
        let _ = libcluu::debug_print(&line);
        0
    } else {
        let pid = reply.words[1];
        let line = format!(
            "suequaltest: FAIL su equal-profile should have been rejected (pid={})",
            pid
        );
        let _ = libcluu::debug_print(&line);
        // Kill the inadvertently spawned session
        let mut kill_msg = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
        kill_msg.words[0] = pid;
        kill_msg.words[1] = 9;
        let _ = libcluu::ipc::call(procmgr_endpoint, &mut kill_msg, libcluu::IpcFlags::empty());
        1
    }
}
