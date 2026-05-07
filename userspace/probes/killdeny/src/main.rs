//! killdeny probe: verifies that kill to a cross-session PID is rejected.
//! Lifted from KillDenyBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::ipc::call;
use libcluu::types::Message;
use libcluu::{registry, Error, IpcFlags};

const PROCMGR_KILL_LABEL: u32 = 3;

fn parse_status(raw: usize) -> libcluu::Result<()> {
    let signed = raw as isize;
    if signed < 0 {
        return Err(Error::from_errno(signed));
    }
    Ok(())
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let target_pid = args
        .get(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2);
    let signal = args
        .get(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(9);

    let procmgr_endpoint = match registry::subscribe_output("procmgr", "spawn") {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("killdeny: FAIL procmgr unavailable {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let mut msg = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
    msg.words[0] = target_pid;
    msg.words[1] = signal;
    if let Err(err) = call(procmgr_endpoint, &mut msg, IpcFlags::empty()) {
        let line = format!("killdeny: FAIL call error {:?}", err);
        let _ = libcluu::debug_print(&line);
        return 1;
    }

    match parse_status(msg.words[0]) {
        Err(Error::PermissionDenied) => {
            let line = format!("killdeny: PASS permission denied pid={}", target_pid);
            let _ = libcluu::debug_print(&line);
            0
        }
        Ok(()) => {
            let line = format!("killdeny: FAIL unexpected success pid={}", target_pid);
            let _ = libcluu::debug_print(&line);
            1
        }
        Err(err) => {
            let line = format!("killdeny: FAIL wrong error {:?} pid={}", err, target_pid);
            let _ = libcluu::debug_print(&line);
            1
        }
    }
}
