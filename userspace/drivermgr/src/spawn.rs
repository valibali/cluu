//! Driver spawn path (D3.2).
//!
//! When `spawn_mode = "spawn"` (or "hybrid"), drivermgr spawns matched
//! drivers via procmgr's `PROCMGR_SPAWN_SERVICE_LABEL`, then registers
//! the driver with drivermon via `DRIVERMON_REGISTER_LABEL`.
//!
//! D3.2 scope: only initrd-based drivers (bind rules with
//! `source_initrd_path`) can be spawned — procmgr's service spawn label
//! only accepts `sys/*` paths from the initrd. Userdisk-based driver
//! loading is D3.6. The spawned driver receives device params (BDF,
//! BARs, IRQ) via ProcessInfo param overrides; token passing (pci_token,
//! irq_token) is not supported by the current spawn label and will be
//! addressed in a future phase.

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;

use libcluu::ipc::{
    call_with_payload, send_msg_with_payload, DRIVERMON_REGISTER_LABEL,
    PARAM_DEVICE_PATH, PARAM_DMA_BASE, PARAM_DMA_PAGES, PARAM_IRQ_LINE,
    PARAM_PCI_BAR0, PARAM_PCI_BDF, PROCMGR_REGISTER_EXIT_NOTIFY_LABEL,
    PROCMGR_SET_FAULT_EP_LABEL, PROCMGR_SPAWN_SERVICE_LABEL,
};
use libcluu::types::Message;
use libcluu::{debug_print, Result};

use crate::bind_rules::BindRule;
use crate::device_tree::{DeviceBus, DeviceNode};

const POLICY_ALWAYS: usize = 0;
const POLICY_ON_FAULT: usize = 2;

const DRIVER_PRIORITY: usize = 100;
const TOKEN_EXTRA_MODE_GRANTABLE: usize = 2;
const PROFILE_SERVICE: usize = 0;

/// Spawn a matched driver via procmgr and register it with drivermon.
///
/// `procmgr_ep` is the pre-resolved root-procmgr:spawn registry endpoint.
/// `drivermon_ep` is the drivermon:main endpoint (for REGISTER).
/// `drivermon_notify_ep` is the drivermon:notify endpoint (for exit-notify
/// registration with procmgr). When non-zero, drivermgr calls
/// PROCMGR_REGISTER_EXIT_NOTIFY_LABEL after spawn so procmgr sends
/// PROC_EXIT_LABEL to drivermon when the driver exits.
/// `drivermon_fault_ep` is the drivermon:fault endpoint (for kernel fault
/// IPC routing). When non-zero, drivermgr calls
/// PROCMGR_SET_FAULT_EP_LABEL after spawn so procmgr sets the spawned
/// thread's fault_endpoint to drivermon's fault endpoint — driver
/// faults then arrive at drivermon for cleanup instead of killing the
/// thread outright.
///
/// Returns `Ok(pid)` on success — the PID is learned from procmgr's
/// synchronous reply to PROCMGR_SPAWN_SERVICE_LABEL. The PID is passed
/// to drivermon in the REGISTER message so exit-notify can correlate.
///
/// If the bind rule has no `source_initrd_path`, the driver cannot be
/// spawned via this label (procmgr rejects non-`sys/` paths). The
/// function logs a warning and returns `Ok(0)` without spawning —
/// userdisk-based driver loading is D3.6.
pub fn spawn_driver(
    rule: &BindRule,
    node: &DeviceNode,
    procmgr_ep: usize,
    drivermon_ep: usize,
    drivermon_notify_ep: usize,
    drivermon_fault_ep: usize,
) -> Result<u32> {
    let initrd_path = match &rule.source_initrd_path {
        Some(p) => p.as_str(),
        None => {
            let _ = debug_print(&format!(
                "drivermgr: spawn {} skipped — no initrd_path (userdisk spawn is D3.6)",
                rule.driver_name
            ));
            return Ok(0);
        }
    };

    let params = build_param_overrides(rule, node);
    let token_requests = build_token_requests(rule);

    let mut payload = Vec::new();
    payload.extend_from_slice(initrd_path.as_bytes());
    payload.push(0);
    for (idx, val) in &params {
        payload.extend_from_slice(&(*idx as u16).to_le_bytes());
        payload.extend_from_slice(&val.to_le_bytes());
    }
    for (slot, rights) in &token_requests {
        payload.extend_from_slice(&(*slot as u16).to_le_bytes());
        payload.extend_from_slice(&rights.to_le_bytes());
    }

    let mut msg = Message::new(
        PROCMGR_SPAWN_SERVICE_LABEL,
        [0; 6],
        5,
    );
    msg.words[0] = payload.len();
    msg.words[1] = DRIVER_PRIORITY;
    msg.words[2] = TOKEN_EXTRA_MODE_GRANTABLE;
    msg.words[3] = params.len();
    msg.words[4] = PROFILE_SERVICE;

    let mut reply = Message::new(0, [0; 6], 0);
    if let Err(e) = call_with_payload(procmgr_ep, &msg, &payload, &mut reply) {
        let _ = debug_print(&format!(
            "drivermgr: spawn {} failed — IPC call error {:?}",
            rule.driver_name, e
        ));
        return Err(e);
    }

    let pid = reply.words[1] as u32;
    let _exit_cookie = reply.words[2];

    let _ = debug_print(&format!(
        "drivermgr: spawned {} for {} (initrd={}) pid={}",
        rule.driver_name, node.path, initrd_path, pid
    ));

    if drivermon_notify_ep != 0 && pid != 0 {
        register_exit_notify(procmgr_ep, pid as usize, drivermon_notify_ep);
    }

    if drivermon_fault_ep != 0 && pid != 0 {
        set_fault_endpoint(procmgr_ep, pid as usize, drivermon_fault_ep);
    }

    let _ = register_with_drivermon(rule, node, pid, drivermon_ep);

    Ok(pid)
}

fn build_param_overrides(rule: &BindRule, node: &DeviceNode) -> Vec<(usize, u64)> {
    let mut params = Vec::new();

    match node.bus {
        DeviceBus::Pci => {
            if let Some((bus, dev, func)) = node.bdf {
                let packed = ((bus as u64) << 16) | ((dev as u64) << 8) | (func as u64);
                params.push((PARAM_DEVICE_PATH, packed));
                params.push((PARAM_PCI_BDF, packed));
            }
            for (i, bar) in node.bars.iter().enumerate() {
                if let Some(addr) = bar {
                    params.push((PARAM_PCI_BAR0 + i, *addr as u64));
                }
            }
            if let Some(irq) = node.irq_line {
                params.push((PARAM_IRQ_LINE, irq as u64));
            }
            if rule.dma {
                params.push((PARAM_DMA_BASE, 0));
                params.push((PARAM_DMA_PAGES, 0));
            }
        }
        DeviceBus::Acpi => {
            if let Some(irq) = node.irq_line {
                params.push((PARAM_IRQ_LINE, irq as u64));
            }
        }
    }

    params
}

fn build_token_requests(rule: &BindRule) -> Vec<(usize, u32)> {
    rule.token_slots.clone()
}

fn register_with_drivermon(
    rule: &BindRule,
    node: &DeviceNode,
    pid: u32,
    drivermon_ep: usize,
) -> Result<()> {
    let policy = if rule.critical {
        POLICY_ALWAYS
    } else {
        POLICY_ON_FAULT
    };
    let has_fallback = 0;

    let mut payload = Vec::new();
    payload.extend_from_slice(node.path.as_bytes());
    payload.push(0);
    payload.extend_from_slice(rule.driver_name.as_bytes());

    let mut msg = Message::new(DRIVERMON_REGISTER_LABEL, [0; 6], 4);
    msg.words[1] = pid as usize;
    msg.words[2] = policy;
    msg.words[3] = has_fallback;

    if let Err(e) = send_msg_with_payload(drivermon_ep, &msg, &payload) {
        let _ = debug_print(&format!(
            "drivermgr: drivermon REGISTER failed {:?} — driver {} unsupervised",
            e, rule.driver_name
        ));
        return Err(e);
    }

    let _ = debug_print(&format!(
        "drivermgr: registered {} (pid={}) with drivermon",
        rule.driver_name, pid
    ));
    Ok(())
}

fn register_exit_notify(procmgr_ep: usize, pid: usize, notify_ep: usize) {
    let msg = Message::new(
        PROCMGR_REGISTER_EXIT_NOTIFY_LABEL,
        [pid, notify_ep, 0, 0, 0, 0],
        2,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    if let Err(e) = call_with_payload(procmgr_ep, &msg, &[], &mut reply) {
        let _ = debug_print(&format!(
            "drivermgr: register_exit_notify pid={} failed {:?}",
            pid, e
        ));
        return;
    }
    if reply.words[0] != 0 {
        let _ = debug_print(&format!(
            "drivermgr: register_exit_notify pid={} errno={}",
            pid, reply.words[0]
        ));
    }
}

fn set_fault_endpoint(procmgr_ep: usize, pid: usize, fault_ep: usize) {
    let msg = Message::new(
        PROCMGR_SET_FAULT_EP_LABEL,
        [pid, fault_ep, 0, 0, 0, 0],
        2,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    if let Err(e) = call_with_payload(procmgr_ep, &msg, &[], &mut reply) {
        let _ = debug_print(&format!(
            "drivermgr: set_fault_ep pid={} failed {:?}",
            pid, e
        ));
        return;
    }
    if reply.words[0] != 0 {
        let _ = debug_print(&format!(
            "drivermgr: set_fault_ep pid={} errno={}",
            pid, reply.words[0]
        ));
    }
}
