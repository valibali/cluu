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
    debug_print("cascadeprobe: start")?;

    registry::init("cascadeprobe")?;
    syscall::yield_cpu()?;

    let procmgr_ep = registry::subscribe_output("procmgr", "spawn")?;
    debug_print("cascadeprobe: got procmgr endpoint")?;

    // Spawn "looper" as a nested child container.
    // looper loops for 30s.  We exit immediately after spawning,
    // triggering procmgr's cascading cleanup which should kill looper.
    let payload = b"looper";
    let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3);
    msg.words[0] = payload.len();
    msg.words[1] = 0; // no notify
    msg.words[2] = 0; // no fdac
    let mut reply = Message::new(0, [0; 6], 0);
    call_with_payload(procmgr_ep, &msg, payload, &mut reply)?;

    let status = reply.words[0];
    let pid = reply.words[1];

    if status == 0 {
        let _ = debug_print(&alloc::format!(
            "cascadeprobe: child looper spawned pid={}, exiting to trigger cascade",
            pid
        ));
    } else {
        let _ = debug_print(&alloc::format!(
            "cascadeprobe: FAIL child spawn failed status={}",
            status
        ));
        return Err(libcluu::Error::Unknown);
    }

    // Exit immediately — procmgr should cascade-kill looper
    Ok(())
}
