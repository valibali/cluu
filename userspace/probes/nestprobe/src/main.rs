#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::ipc::{call_with_payload, PROCMGR_CONTAINER_RUN_LABEL};
use libcluu::types::Message;
use libcluu::{debug_print, registry, syscall};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

fn run() -> libcluu::Result<()> {
    debug_print("nestprobe: start")?;

    registry::init("nestprobe")?;
    syscall::yield_cpu()?;

    let procmgr_ep = registry::subscribe_output("procmgr", "spawn")?;
    debug_print("nestprobe: got procmgr endpoint")?;

    // Spawn "hello" as a nested container
    let payload = b"hello";
    let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3);
    msg.words[0] = payload.len();
    msg.words[1] = 0; // no notify
    msg.words[2] = 0; // no fdac
    let mut reply = Message::new(0, [0; 6], 0);
    call_with_payload(procmgr_ep, &msg, payload, &mut reply)?;

    let status = reply.words[0];
    let pid = reply.words[1];

    if status == 0 {
        let _ = debug_print(&alloc::format!("nestprobe: PASS nested spawn pid={}", pid));
    } else {
        let _ = debug_print(&alloc::format!("nestprobe: FAIL status={}", status));
        return Err(libcluu::Error::Unknown);
    }

    Ok(())
}
