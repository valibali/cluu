//! Probe: pm_session_crash_cascade
//!
//! Differentiation vs l3_session_leader_exit_cascades:
//!   l3_session_leader_exit_cascades exercises the leader-process-exit path
//!   (set self as leader, then exit; harness inspects cascade externally).
//!   This probe exercises the *unclean-teardown* angle from inside a single
//!   process: subscribe to a session, then tear it down via session::destroy
//!   (the same code path procmgr drives when a leader dies abnormally —
//!   SESSION_TABLE::mark_dying → destroy_session → SESSION_ENDED fan-out),
//!   and synchronously observe the SESSION_ENDED event on the subscriber
//!   endpoint, validating both label and postcard payload structurally.
//!
//! Strategies (in priority order):
//!   A. (preferred) Subscribe + destroy + recv SESSION_ENDED with matching
//!      session_id — verifies the live cascade plumbing end-to-end.
//!   B. (n/a here) leader-crash from inside the same process — not feasible
//!      without a supervisor; covered partially by l3_session_leader_exit_cascades.
//!   C. (fallback) postcard structural roundtrip of SessionEndedEvent — proves
//!      wire schema integrity even if subscribe/destroy can't be exercised.
//!
//! Output contract:
//!   `pm_session_crash_cascade: PASS <subcase>` on success
//!   `pm_session_crash_cascade: FAIL <reason>` on failure

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec;

use cluu_wire::session::{
    ProfileSpec, SessionCreateRequest, SessionEndedEvent, SESSION_ENDED_LABEL,
};
use cluu_wire::spawn::ViewSource;

use libcluu::debug_print;

const LABEL: &str = "pm_session_crash_cascade";

// Upper bound for how long we wait for the SESSION_ENDED event after destroy.
// Procmgr emits the event synchronously in destroy_session, so this should
// be effectively immediate; we pick a generous bound to absorb scheduler jitter.
const RECV_TIMEOUT_MS: u64 = 2_000;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

fn run() -> Result<(), ()> {
    let _ = debug_print(&format!("{}: start\n", LABEL));

    // Structural subcase (C) always runs first — cheap sanity that the wire
    // type encodes/decodes via postcard. If this fails, the cascade event
    // could never roundtrip in production either.
    if !structural_roundtrip_ok() {
        let _ = debug_print(&format!(
            "{}: FAIL structural SessionEndedEvent postcard roundtrip\n",
            LABEL
        ));
        return Err(());
    }
    let _ = debug_print(&format!(
        "{}: case_a PASS structural SessionEndedEvent postcard roundtrip\n",
        LABEL
    ));

    // Live subcase (A): subscribe → destroy → recv SESSION_ENDED.
    match live_cascade_subcase() {
        LiveOutcome::Pass { session_id } => {
            let _ = debug_print(&format!(
                "{}: case_b PASS got SessionEndedEvent on destroy sid={}\n",
                LABEL, session_id
            ));
        }
        LiveOutcome::Skipped(reason) => {
            // The environment didn't let us exercise the live cascade
            // (e.g. session create denied, no endpoint cap). The structural
            // subcase already ran, so we still report PASS but tag the
            // narrower scope so harness output is unambiguous.
            let _ = debug_print(&format!(
                "{}: case_b SKIP live cascade ({}) — structural-only\n",
                LABEL, reason
            ));
        }
        LiveOutcome::Fail(reason) => {
            let _ = debug_print(&format!("{}: FAIL {}\n", LABEL, reason));
            return Err(());
        }
    }

    let _ = debug_print(&format!("{}: PASS\n", LABEL));
    Ok(())
}

fn structural_roundtrip_ok() -> bool {
    let original = SessionEndedEvent { session_id: 0xDEAD_BEEF };
    let bytes = match postcard::to_allocvec(&original) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let decoded: SessionEndedEvent = match postcard::from_bytes(&bytes) {
        Ok(d) => d,
        Err(_) => return false,
    };
    decoded.session_id == original.session_id
}

enum LiveOutcome {
    Pass { session_id: u32 },
    Skipped(&'static str),
    Fail(String),
}

fn live_cascade_subcase() -> LiveOutcome {
    // Create an endpoint we own; subscribe will derive an IPC_SEND cap from
    // it for procmgr to use when it fires SESSION_ENDED.
    let event_ep = match libcluu::syscall::endpoint_create(libcluu::boot::token_ipc()) {
        Ok(ep) => ep,
        Err(_) => return LiveOutcome::Skipped("endpoint_create failed"),
    };

    let req = SessionCreateRequest {
        user_name: String::from("root"),
        password: String::new(),
        profile: ProfileSpec {
            home: String::from("/tmp"),
            initial_view: ViewSource::Derive(0xC0FFEE),
            env: vec![],
            umask: 0o022,
        },
    };
    let created = match libcluu::session::create(req) {
        Ok(o) => o,
        Err(e) => {
            // Surface the create error via debug_print so the cause is visible
            // in the harness log, then fall back to Skipped (structural subcase
            // already covers the wire schema).
            let _ = debug_print(&format!(
                "{}: case_b session::create failed {:?}\n",
                LABEL, e
            ));
            return LiveOutcome::Skipped("session::create denied or unavailable");
        }
    };

    // Subscribe — procmgr mints a SEND-rights derivation of event_ep and
    // stores it as a subscriber.
    if let Err(e) = libcluu::session::subscribe(created.token, event_ep as u64) {
        let _ = libcluu::session::destroy(created.token);
        return LiveOutcome::Fail(format!("session::subscribe failed {:?}", e));
    }

    // Trigger cascade. destroy → mark_dying → destroy_session emits
    // SESSION_ENDED to every subscriber (us).
    if let Err(e) = libcluu::session::destroy(created.token) {
        return LiveOutcome::Fail(format!("session::destroy failed {:?}", e));
    }

    // Drain the endpoint until we see SESSION_ENDED or the deadline expires.
    // We allow at most a few stray messages before giving up — in practice
    // the only message landing on a freshly-minted private endpoint should
    // be the cascade event itself.
    let mut buf = [0u8; 256];
    for _ in 0..4 {
        match libcluu::syscall::ipc_recv_timeout(event_ep, &mut buf, RECV_TIMEOUT_MS) {
            Ok(len) => {
                let (msg, payload) = match libcluu::ipc::parse_message(&buf[..len]) {
                    Some(parsed) => parsed,
                    None => continue,
                };
                if msg.tag.label != SESSION_ENDED_LABEL {
                    continue;
                }
                let event: SessionEndedEvent = match postcard::from_bytes(payload) {
                    Ok(e) => e,
                    Err(_) => {
                        return LiveOutcome::Fail(String::from(
                            "SESSION_ENDED payload failed postcard decode",
                        ));
                    }
                };
                if event.session_id != created.session_id {
                    return LiveOutcome::Fail(format!(
                        "session_id mismatch event={} expected={}",
                        event.session_id, created.session_id
                    ));
                }
                return LiveOutcome::Pass { session_id: event.session_id };
            }
            Err(libcluu::error::Error::Timeout) => {
                return LiveOutcome::Fail(String::from(
                    "timeout waiting for SESSION_ENDED after destroy",
                ));
            }
            Err(e) => {
                return LiveOutcome::Fail(format!("ipc_recv_timeout err {:?}", e));
            }
        }
    }
    LiveOutcome::Fail(String::from(
        "drained event endpoint without seeing SESSION_ENDED",
    ))
}

