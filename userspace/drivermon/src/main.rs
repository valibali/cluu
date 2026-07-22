#![no_std]
#![no_main]

//! drivermon — CLUU driver monitor.
//!
//! Supervises driver processes spawned by drivermgr. Handles exit
//! notifications, restart per policy, fallback chain walking, and fault
//! IPC. Publishes driver state transitions back to drivermgr.
//!
//! Phase D1 skeleton: registry init + recv loop. D1.6 notify endpoint
//! created and registered. D3.3: REGISTER/RESPAWN/REBIND IPC from
//! drivermgr updates the runtime table. D4 will wire exit-notify and
//! fault IPC into the recv loop.
//!
//! Architecture (SOLID):
//! - `runtime_table.rs` — domain types (RuntimeEntry, DriverRuntimeTable). No deps.
//! - `handlers.rs`      — one handler per supervision label (D3.3).
//! - `main.rs`          — orchestration: init, recv, dispatch. No business logic.
//!
//! Sync recv loop: drivermon is a skeleton leaf service for now. When it
//! gains downstream IPC (procmgr, drivermgr, kernel fault endpoint), it
//! will adopt the async runtime per AGENTS.md §7.

extern crate alloc;

mod handlers;
mod runtime_table;
mod supervision;

use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_IPC};
use libcluu::ipc::{
    parse_message, reply_to_sender, DRIVERMON_REBIND_LABEL, DRIVERMON_REGISTER_LABEL,
    DRIVERMON_RESPAWN_LABEL, FAULT_LABEL, PROC_EXIT_LABEL,
};
use libcluu::registry;
use libcluu::syscall;
use libcluu::syscall::ipc_recv_any_with_sender;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Result};

use runtime_table::DriverRuntimeTable;

const REPLY_OK: usize = 0;

/// Index of the registry control endpoint in the recv `tokens` array.
const REGISTRY_TOKEN_IDX: usize = 1;

/// Index of the notify endpoint in the recv `tokens` array.
const NOTIFY_TOKEN_IDX: usize = 2;

/// Index of the fault endpoint in the recv `tokens` array.
const FAULT_TOKEN_IDX: usize = 3;

/// Reply label sent to the kernel fault endpoint meaning KILL the
/// faulting thread (label != 0 → KILL per `handle_fault_reply`).
const FAULT_REPLY_KILL: u32 = 1;

static RUNTIME_TABLE: spin::Mutex<DriverRuntimeTable> = spin::Mutex::new(DriverRuntimeTable::new());

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    let info = process_info();
    let main_endpoint = info.tokens[TOKEN_EXTRA_0];

    // Mint a second endpoint for exit-notify traffic. procmgr will send
    // PROC_EXIT_LABEL here in D4; keeping it separate from `main` lets
    // the recv loop distinguish control RPCs from one-way notifications
    // by index alone.
    let notify_endpoint = syscall::endpoint_create(info.tokens[TOKEN_IPC])?;

    // Mint a third endpoint for kernel fault IPC. When a supervised
    // driver thread faults and its fault_endpoint was set to this
    // endpoint by procmgr (via PROCMGR_SET_FAULT_EP_LABEL), the kernel
    // delivers a FAULT_LABEL message here. drivermon replies with a
    // kill directive so the kernel cleans up the thread; the actual
    // restart goes through the exit-notify path (D4.1).
    let fault_endpoint = syscall::endpoint_create(info.tokens[TOKEN_IPC])?;

    registry::init("drivermon")?;
    registry::register_output("main", main_endpoint)?;
    registry::register_output("notify", notify_endpoint)?;
    registry::register_output("fault", fault_endpoint)?;
    let _ = debug_print("drivermon: ready");
    let _ = debug_print("drivermon: notify endpoint ready");
    let _ = debug_print("drivermon: fault endpoint ready");

    let control_endpoint = registry::control_endpoint();
    let mut buf = [0u8; 512];

    loop {
        let tokens = [main_endpoint, control_endpoint, notify_endpoint, fault_endpoint];
        match ipc_recv_any_with_sender(&tokens, &mut buf, u64::MAX) {
            Ok((idx, len, _sender_tid)) => {
                if len < core::mem::size_of::<Message>() {
                    continue;
                }
                if idx == REGISTRY_TOKEN_IDX {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let _ = registry::handle_incoming_message(&msg, payload);
                    }
                    continue;
                }
                if idx == FAULT_TOKEN_IDX {
                    handle_fault_ipc(&buf[..len], fault_endpoint);
                    continue;
                }
                let (msg, payload) = match parse_message(&buf[..len]) {
                    Some(parsed) => parsed,
                    None => continue,
                };
                let label = msg.tag.label;
                if idx == NOTIFY_TOKEN_IDX {
                    if label == PROC_EXIT_LABEL {
                        let action = {
                            let mut table = RUNTIME_TABLE.lock();
                            supervision::handle_exit_notify(&mut table, &msg)
                        };
                        if let Some(action) = action {
                            supervision::send_action(action);
                        }
                    } else {
                        let _ = debug_print(&alloc::format!(
                            "drivermon: notify label=0x{:x}",
                            label
                        ));
                    }
                    continue;
                }
                if !handle_supervision(label, &msg, payload, main_endpoint) {
                    let _ = debug_print(&alloc::format!(
                        "drivermon: recv label=0x{:x}",
                        label
                    ));
                    let reply_msg = Message::new(label, [REPLY_OK, 0, 0, 0, 0, 0], 1);
                    let _ = reply_to_sender(&msg, &reply_msg, main_endpoint, IpcFlags::empty());
                }
            }
            Err(libcluu::Error::WouldBlock) => {
                let _ = yield_cpu();
            }
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    }
}

/// Handle a kernel fault IPC message (label 0xFA017).
///
/// The kernel sends a fault message when a supervised driver thread
/// faults. `words[5]` is the reply_id: if non-zero, the kernel is
/// waiting for a reply — we reply with label != 0 (KILL) so the kernel
/// cleans up the thread. If `words[5]` is 0, the thread is already
/// dead and no reply is needed. Restart goes through the existing
/// exit-notify path (D4.1).
fn handle_fault_ipc(buf: &[u8], fault_endpoint: usize) {
    let msg = match parse_message(buf) {
        Some((m, _)) => m,
        None => return,
    };
    if msg.tag.label != FAULT_LABEL {
        let _ = debug_print(&alloc::format!(
            "drivermon: fault endpoint recv label=0x{:x}",
            msg.tag.label
        ));
        return;
    }
    let fault_type = msg.words[0];
    let fault_addr = msg.words[1];
    let tid = msg.words[4];
    let reply_id = msg.words[5];
    let _ = debug_print(&alloc::format!(
        "drivermon: fault tid={} type={} addr=0x{:x} reply_id={}",
        tid, fault_type, fault_addr, reply_id
    ));
    if reply_id != 0 {
        let reply = Message::new(FAULT_REPLY_KILL, [0; 6], 0);
        if let Err(e) = reply_to_sender(&msg, &reply, fault_endpoint, IpcFlags::empty()) {
            let _ = debug_print(&alloc::format!(
                "drivermon: fault reply failed tid={} err={:?}",
                tid, e
            ));
        }
    }
}

/// Dispatch drivermgr supervision labels to the D3.3 handlers.
/// Returns true if `label` was recognized (and a reply was sent).
fn handle_supervision(label: u32, msg: &Message, payload: &[u8], ep: usize) -> bool {
    match label {
        DRIVERMON_REGISTER_LABEL => {
            let mut table = RUNTIME_TABLE.lock();
            handlers::handle_register(&mut table, msg, payload, ep);
            true
        }
        DRIVERMON_RESPAWN_LABEL => {
            let mut table = RUNTIME_TABLE.lock();
            handlers::handle_respawn(&mut table, msg, payload, ep);
            true
        }
        DRIVERMON_REBIND_LABEL => {
            let mut table = RUNTIME_TABLE.lock();
            handlers::handle_rebind(&mut table, msg, payload, ep);
            true
        }
        _ => false,
    }
}
