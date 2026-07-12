//! jobmix probe: deterministic two-job stop/bg/fg-style interleaving stress.
//! Lifted from JobMixBuiltin (jobs.rs).
//!
//! Calls procmgr directly for spawn + signal + wait.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::boot::{process_info, TOKEN_IPC};
use libcluu::ipc::{call, call_with_payload, recv, PROCMGR_CONTAINER_RUN_LABEL};
use libcluu::types::Message;
use libcluu::{registry, IpcFlags};

const PROCMGR_KILL_LABEL: u32 = 3;

fn parse_status(raw: usize) -> libcluu::Result<()> {
    let signed = raw as isize;
    if signed < 0 {
        return Err(libcluu::Error::from_errno(signed));
    }
    Ok(())
}

fn signal_process(procmgr_ep: usize, pid: usize, signal: usize) -> libcluu::Result<()> {
    let mut req = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
    req.words[0] = pid;
    req.words[1] = signal;
    call(procmgr_ep, &mut req, IpcFlags::empty())?;
    parse_status(req.words[0])
}

fn spawn_sleepy(procmgr_ep: usize) -> libcluu::Result<(usize, usize)> {
    let name = b"sleepy";
    let notify_endpoint =
        libcluu::syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
    let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3);
    msg.words[0] = name.len();
    msg.words[1] = notify_endpoint;
    msg.words[2] = 0;
    let mut reply = Message::new(0, [0; 6], 0);
    call_with_payload(procmgr_ep, &msg, name, &mut reply)?;
    parse_status(reply.words[0])?;
    Ok((reply.words[1], notify_endpoint))
}

fn wait_for_exit(notify_endpoint: usize) {
    let mut msg = Message::new(0, [0; 6], 0);
    let _ = recv(notify_endpoint, &mut msg, IpcFlags::empty());
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let procmgr_ep = match registry::subscribe_output("root-procmgr", "spawn") {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("jobmix: FAIL procmgr unavailable {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let (pid_a, notify_a) = match spawn_sleepy(procmgr_ep) {
        Ok(r) => r,
        Err(err) => {
            let line = format!("jobmix: FAIL spawn_a {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let (pid_b, notify_b) = match spawn_sleepy(procmgr_ep) {
        Ok(r) => r,
        Err(err) => {
            let line = format!("jobmix: FAIL spawn_b {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    if let Err(err) = signal_process(procmgr_ep, pid_a, 19 /* SIGSTOP */) {
        let line = format!("jobmix: FAIL sigstop_a {:?}", err);
        let _ = libcluu::debug_print(&line);
        return 1;
    }

    if let Err(err) = signal_process(procmgr_ep, pid_b, 19 /* SIGSTOP */) {
        let line = format!("jobmix: FAIL sigstop_b {:?}", err);
        let _ = libcluu::debug_print(&line);
        return 1;
    }

    if let Err(err) = signal_process(procmgr_ep, pid_a, 18 /* SIGCONT */) {
        let line = format!("jobmix: FAIL sigcont_a {:?}", err);
        let _ = libcluu::debug_print(&line);
        return 1;
    }

    if let Err(err) = signal_process(procmgr_ep, pid_b, 18 /* SIGCONT */) {
        let line = format!("jobmix: FAIL sigcont_b {:?}", err);
        let _ = libcluu::debug_print(&line);
        return 1;
    }

    wait_for_exit(notify_b);
    wait_for_exit(notify_a);

    let line = format!("jobmix: PASS pids={},{}", pid_a, pid_b);
    let _ = libcluu::debug_print(&line);
    0
}
