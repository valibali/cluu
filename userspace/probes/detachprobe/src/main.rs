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
    debug_print("detachprobe: start")?;

    registry::init("detachprobe")?;
    syscall::yield_cpu()?;

    let procmgr_ep = registry::subscribe_output("procmgr", "spawn")?;
    debug_print("detachprobe: got procmgr endpoint")?;

    // Spawn "survivor" as a nested child container.
    // survivor has DETACH in its Cluufile, so parent_container_id=0.
    // When we exit, cascading cleanup should NOT kill survivor.
    let payload = b"survivor";
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
            "detachprobe: child survivor spawned pid={}, exiting",
            pid
        ));
    } else {
        let _ = debug_print(&alloc::format!(
            "detachprobe: FAIL child spawn failed status={}",
            status
        ));
        return Err(libcluu::Error::Unknown);
    }

    // Exit immediately — survivor should survive because it's detached
    Ok(())
}
