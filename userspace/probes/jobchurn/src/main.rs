//! jobchurn probe: repeated stop/resume/foreground cycles with telemetry.
//! Lifted from JobChurnBuiltin (jobs.rs).
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
#[allow(dead_code)]
// rationale: priority constant for future spawn-with-priority testing.
const DEFAULT_PRIORITY: usize = 200;

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
    let args = libcluu::args::args();
    let iterations = args
        .get(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(3);

    if iterations == 0 {
        let _ = libcluu::debug_print("jobchurn: iterations must be >= 1");
        return 1;
    }

    let procmgr_ep = match registry::subscribe_output("root-procmgr", "spawn") {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("jobchurn: FAIL procmgr unavailable {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    for _ in 0..iterations {
        let (pid, notify_ep) = match spawn_sleepy(procmgr_ep) {
            Ok(r) => r,
            Err(err) => {
                let line = format!("jobchurn: FAIL spawn {:?}", err);
                let _ = libcluu::debug_print(&line);
                return 1;
            }
        };

        if let Err(err) = signal_process(procmgr_ep, pid, 19 /* SIGSTOP */) {
            let line = format!("jobchurn: FAIL sigstop {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }

        if let Err(err) = signal_process(procmgr_ep, pid, 18 /* SIGCONT */) {
            let line = format!("jobchurn: FAIL sigcont {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }

        wait_for_exit(notify_ep);
    }

    let line = format!("jobchurn: PASS iterations={}", iterations);
    let _ = libcluu::debug_print(&line);
    0
}
