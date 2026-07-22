//! Exit-notify supervision logic (D4).
//!
//! Decodes PROC_EXIT_LABEL from procmgr, applies restart policy,
//! enforces the restart budget, walks the fallback chain, and returns
//! a deferred action for the caller to send to drivermgr. The caller
//! releases the RUNTIME_TABLE lock before sending the IPC.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libcluu::boot::process_info;
use libcluu::ipc::{
    send_msg_with_payload, DRIVERMGR_DEVICE_STATE_LABEL, DRIVERMGR_RESPAWN_DEVICE_LABEL,
};
use libcluu::registry;
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{debug_print, yield_cpu};

use crate::runtime_table::{DriverRuntimeTable, RestartPolicy};

static DRIVERMGR_EP: spin::Mutex<usize> = spin::Mutex::new(0);

/// Device state word sent in DRIVERMGR_DEVICE_STATE_LABEL.words[1].
/// Mirrors the DriverState ordering in runtime_table.rs.
pub const DEVICE_STATE_BOUND: usize = 0;
pub const DEVICE_STATE_RESTARTING: usize = 1;
pub const DEVICE_STATE_FAILED: usize = 2;

fn resolve_drivermgr_ep() -> usize {
    let mut ep = DRIVERMGR_EP.lock();
    if *ep != 0 {
        return *ep;
    }
    match registry::subscribe_output("drivermgr", "main") {
        Ok(resolved) => {
            *ep = resolved;
            resolved
        }
        Err(e) => {
            let _ = debug_print(&format!(
                "drivermon: drivermgr:main subscribe failed {:?}",
                e
            ));
            0
        }
    }
}

fn now_ms() -> u64 {
    let info = process_info();
    let clock_token = info.tokens[libcluu::boot::TOKEN_CLOCK];
    let ticks = syscall::clock_now(clock_token).unwrap_or(0);
    let freq = syscall::clock_frequency(clock_token).unwrap_or(1_000_000_000);
    if freq == 0 {
        return ticks;
    }
    ticks.saturating_mul(1000) / freq
}

/// Deferred IPC action returned by `handle_exit_notify` for the caller
/// to send after releasing the RUNTIME_TABLE lock.
pub enum DeferredAction {
    /// Send DRIVERMGR_RESPAWN_DEVICE_LABEL with optional fallback image.
    Respawn {
        device_path: String,
        fallback: Option<String>,
    },
    /// Send DRIVERMGR_DEVICE_STATE_LABEL with the new state.
    DeviceState {
        device_path: String,
        state: usize,
    },
}

/// Handle PROC_EXIT_LABEL from procmgr.
///
/// Wire format (extended D4): words[0]=exit_cookie, words[1]=exit_code,
/// words[2]=pid. The PID is used to look up the RuntimeEntry.
///
/// Returns `Some(DeferredAction)` if the caller should send an IPC to
/// drivermgr after releasing the table lock. Returns `None` if no
/// action is needed (not in table, clean exit with OnFault, etc.).
/// In the boot-critical-exhausted case, this function never returns —
/// it loops forever so init panics when drivermon dies.
pub fn handle_exit_notify(table: &mut DriverRuntimeTable, msg: &Message) -> Option<DeferredAction> {
    let pid = msg.words[2] as u32;
    let exit_code = msg.words[1] as i32;

    if pid == 0 {
        let _ = debug_print(&format!(
            "drivermon: exit_notify pid=0 cookie={} code={} — no PID, ignoring",
            msg.words[0], exit_code
        ));
        return None;
    }

    let snapshot = match table.get(&pid) {
        Some(e) => ExitSnapshot {
            device_path: e.device_path.clone(),
            driver_image: e.driver_image.clone(),
            policy: e.policy,
            fallback: e.fallback.clone(),
            restart_count: e.restart_count,
            max_restarts: e.max_restarts,
            visited_fallbacks: e.visited_fallbacks.clone(),
        },
        None => {
            let _ = debug_print(&format!(
                "drivermon: exit_notify pid={} code={} — not in runtime table",
                pid, exit_code
            ));
            return None;
        }
    };

    let _ = debug_print(&format!(
        "drivermon: exit pid={} device={} code={} policy={:?} restarts={}/{}",
        pid, snapshot.device_path, exit_code, snapshot.policy,
        snapshot.restart_count, snapshot.max_restarts
    ));

    let clean_exit = exit_code == 0;
    let want_restart = match snapshot.policy {
        RestartPolicy::Always => true,
        RestartPolicy::Never => false,
        RestartPolicy::OnFault => !clean_exit,
    };

    if !want_restart {
        table.remove(&pid);
        let _ = debug_print(&format!(
            "drivermon: driver {} (pid={}) exited, policy={:?} — not restarting",
            snapshot.driver_image, pid, snapshot.policy
        ));
        return None;
    }

    let now = now_ms();
    let within_budget = table.check_restart_budget(&pid, now);

    if within_budget {
        table.bump_restart(&pid, now);
        let _ = debug_print(&format!(
            "drivermon: restart driver {} for device {} (count {})",
            snapshot.driver_image, snapshot.device_path, snapshot.restart_count + 1
        ));
        return Some(DeferredAction::Respawn {
            device_path: snapshot.device_path,
            fallback: None,
        });
    }

    let _ = debug_print(&format!(
        "drivermon: restart budget exceeded for {} (pid={}) — trying fallback",
        snapshot.driver_image, pid
    ));

    match &snapshot.fallback {
        Some(fallback) if !snapshot.visited_fallbacks.contains(fallback) => {
            table.add_visited_fallback(&pid, fallback);
            table.mark_restarting(&pid);
            let _ = debug_print(&format!(
                "drivermon: fallback to {} for device {}",
                fallback, snapshot.device_path
            ));
            Some(DeferredAction::Respawn {
                device_path: snapshot.device_path,
                fallback: Some(fallback.clone()),
            })
        }
        Some(_) => {
            table.mark_failed(&pid);
            let _ = debug_print(&format!(
                "drivermon: fallback chain exhausted for {} (pid={}) — marked Failed",
                snapshot.driver_image, pid
            ));
            Some(DeferredAction::DeviceState {
                device_path: snapshot.device_path,
                state: DEVICE_STATE_FAILED,
            })
        }
        None => {
            if snapshot.policy == RestartPolicy::Always {
                let _ = debug_print(&format!(
                    "drivermon: FATAL boot-critical driver {} (pid={}) exhausted restart budget with no fallback — looping",
                    snapshot.driver_image, pid
                ));
                loop {
                    let _ = yield_cpu();
                }
            }
            table.mark_failed(&pid);
            let _ = debug_print(&format!(
                "drivermon: no fallback for {} (pid={}) — marked Failed",
                snapshot.driver_image, pid
            ));
            Some(DeferredAction::DeviceState {
                device_path: snapshot.device_path,
                state: DEVICE_STATE_FAILED,
            })
        }
    }
}

/// Send a deferred action to drivermgr. Called after releasing the
/// RUNTIME_TABLE lock.
pub fn send_action(action: DeferredAction) {
    match action {
        DeferredAction::Respawn { device_path, fallback } => {
            send_device_state(&device_path, DEVICE_STATE_RESTARTING);
            send_respawn(&device_path, fallback.as_deref());
        }
        DeferredAction::DeviceState { device_path, state } => {
            send_device_state(&device_path, state);
        }
    }
}

/// Notify drivermgr of a device state transition (D4.5). Fire-and-forget.
pub fn send_device_state(device_path: &str, state: usize) {
    let ep = resolve_drivermgr_ep();
    if ep == 0 {
        let _ = debug_print(
            "drivermon: cannot send DEVICE_STATE — drivermgr:main unresolved",
        );
        return;
    }
    let mut msg = Message::new(DRIVERMGR_DEVICE_STATE_LABEL, [0; 6], 2);
    msg.words[1] = state;
    if let Err(e) = send_msg_with_payload(ep, &msg, device_path.as_bytes()) {
        let _ = debug_print(&format!(
            "drivermon: DEVICE_STATE send failed {:?} — device {} state {} not reflected",
            e, device_path, state
        ));
    }
}

struct ExitSnapshot {
    device_path: String,
    driver_image: String,
    policy: RestartPolicy,
    fallback: Option<String>,
    restart_count: u32,
    max_restarts: u32,
    visited_fallbacks: Vec<String>,
}

fn send_respawn(device_path: &str, fallback: Option<&str>) {
    let ep = resolve_drivermgr_ep();
    if ep == 0 {
        let _ = debug_print("drivermon: cannot send RESPAWN_DEVICE — drivermgr:main unresolved");
        return;
    }

    let mut payload = Vec::new();
    payload.extend_from_slice(device_path.as_bytes());
    if let Some(fb) = fallback {
        payload.push(0);
        payload.extend_from_slice(fb.as_bytes());
    }

    let driver_image_len = fallback.map(|s| s.len()).unwrap_or(0);
    let mut msg = Message::new(DRIVERMGR_RESPAWN_DEVICE_LABEL, [0; 6], 2);
    msg.words[0] = payload.len();
    msg.words[1] = driver_image_len;

    if let Err(e) = send_msg_with_payload(ep, &msg, &payload) {
        let _ = debug_print(&format!(
            "drivermon: RESPAWN_DEVICE send failed {:?} — device {} stays unbound",
            e, device_path
        ));
    }
}
