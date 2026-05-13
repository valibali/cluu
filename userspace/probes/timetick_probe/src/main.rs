//! Periodic-tick subscription smoke test.
//!
//! Subscribes to the timeserver with period_ms=100, counts 10 TIME_TICK
//! push messages, then prints `TIMETICK_PROBE: count=10` and exits.
//! Maps to harness marker mode `l2_timeserver_pushmode_tick`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use core::mem::size_of;

#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::boot::token_ipc;
use libcluu::syscall::{endpoint_create, ipc_recv_any};
use libcluu::time::{TIME_SUBSCRIBE_PERIODIC_LABEL, TIME_TICK_LABEL, TIME_UNSUBSCRIBE_LABEL};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry};

fn fail(msg: &str) -> i32 {
    let _ = debug_print(&format!("TIMETICK_PROBE: FAIL {}", msg));
    1
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // Init registry so subscribe_output can resolve "timeserver".
    if registry::init("timetick_probe").is_err() {
        return fail("registry::init");
    }
    let _ = libcluu::syscall::yield_cpu();

    // Resolve the timeserver endpoint.
    let time_ep = match registry::subscribe_output("timeserver", "main") {
        Ok(ep) => ep,
        Err(_) => return fail("subscribe_output(timeserver:main)"),
    };

    // Create a local receive endpoint for push notifications.
    let ipc_cap = token_ipc();
    let notify_ep = match endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => return fail("endpoint_create"),
    };

    // Send TIME_SUBSCRIBE_PERIODIC.
    // Wire: words[0]=period_ms  words[1]=notify_ep
    let mut msg = Message::new(
        TIME_SUBSCRIBE_PERIODIC_LABEL,
        [100, notify_ep, 0, 0, 0, 0],
        2,
    );
    if libcluu::ipc::call(time_ep, &mut msg, IpcFlags::empty()).is_err() {
        return fail("subscribe ipc::call");
    }
    if msg.words[0] != 0 {
        return fail(&format!("subscribe errno={}", msg.words[0]));
    }

    // Receive 10 TIME_TICK push messages.
    let mut count: u64 = 0;
    let mut buf = [0u8; size_of::<Message>()];
    while count < 10 {
        // Wait up to 5 s per tick (100 ms × 10 with margin).
        match ipc_recv_any(&[notify_ep], &mut buf, 5_000) {
            Ok((_idx, len)) => {
                if len < size_of::<Message>() {
                    continue;
                }
                let recv_msg =
                    unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
                if recv_msg.tag.label == TIME_TICK_LABEL {
                    count += 1;
                }
            }
            Err(_) => {
                return fail("ipc_recv_any timeout or error");
            }
        }
    }

    let _ = debug_print(&format!("TIMETICK_PROBE: count={}", count));

    // Best-effort unsubscribe.
    let mut unsub = Message::new(TIME_UNSUBSCRIBE_LABEL, [0; 6], 0);
    let _ = libcluu::ipc::call(time_ep, &mut unsub, IpcFlags::empty());

    0
}
