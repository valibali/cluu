//! Probe: pm_proc_query_all_cap
//!
//! Verifies that the `PROC_QUERY_ALL` IPC verb (root-procmgr's
//! "list all processes in the system" aggregator) cannot be invoked by a
//! default, unprivileged container.
//!
//! Source of truth for the label and cap:
//!   - `userspace/libs/procmgr-common/src/labels.rs`:
//!         `PROCMGR_PROC_QUERY_ALL_LABEL = 0xA003`
//!   - `userspace/root-procmgr/src/proc_query_all.rs`:
//!         `SYSTEM_PROC_QUERY_CAP_ID = 0xCAFE_0000_0000_0001`
//!         (gates the handler; missing/wrong cap => `HandlerError::BadCap`)
//!
//! Current wire status (2026-05-27):
//!   The handler `ProcQueryAll` is defined in `root-procmgr` and wired into
//!   the TEST dispatcher (`dispatch.rs`), but the live `main.rs` recv loop
//!   does NOT route label `0xA003`. An unknown label falls through to
//!   `handle_spawn_message`, which rejects the caller with
//!   `Error::PermissionDenied` because this container's profile does not
//!   carry `CapProfile::SPAWN`. Either outcome (handler-cap deny via
//!   `BadCap` once wired, OR fallback-path deny today) satisfies the
//!   security invariant this probe asserts: an unprivileged container
//!   MUST NOT be able to retrieve a system-wide process listing.
//!
//! This probe deliberately presents `words[0] = 0` (no cap) to ensure that
//! when the live wiring lands, the deny still fires through the cap gate.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::ipc::call_with_payload;
use libcluu::types::Message;
use libcluu::{debug_print, registry};

const LABEL: &str = "pm_proc_query_all_cap";

/// Mirrors `procmgr_common::labels::PROCMGR_PROC_QUERY_ALL_LABEL`.
/// Re-declared here to keep the probe's dep-graph minimal (libcluu only).
const PROCMGR_PROC_QUERY_ALL_LABEL: u32 = 0xA003;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

fn run() -> Result<(), ()> {
    let _ = debug_print(&format!("{}: start\n", LABEL));

    // Resolve root-procmgr's main IPC endpoint. Root-procmgr registers
    // itself under `procmgr:spawn` (and `procmgr:session` aliased to the
    // same endpoint). Both names land on the same recv loop, which
    // dispatches by label.
    let procmgr_ep = match registry::lookup_service("procmgr:spawn") {
        Some(ep) => ep,
        None => {
            let _ = debug_print(&format!(
                "{}: FAIL procmgr:spawn lookup failed\n",
                LABEL
            ));
            return Err(());
        }
    };
    let _ = debug_print(&format!("{}: got procmgr ep\n", LABEL));

    // Build a PROC_QUERY_ALL request with NO cap presented.
    //   words[0] = 0  → SYSTEM_PROC_QUERY_CAP_ID is 0xCAFE..., 0 must be
    //                   rejected by the cap gate when the verb is live.
    // Payload is empty: the handler reads only the cap word.
    let mut msg = Message::new(PROCMGR_PROC_QUERY_ALL_LABEL, [0; 6], 1);
    msg.words[0] = 0;

    let mut reply = Message::new(0, [0; 6], 0);
    let send_res = call_with_payload(procmgr_ep, &msg, &[], &mut reply);

    match send_res {
        Ok(()) => {
            // The call returned a reply. Inspect it to decide PASS/FAIL.
            // Convention across procmgr handlers: words[0] carries the
            // errno (negative on error, 0 on success). A non-zero status
            // means the request was rejected — that is the desired
            // outcome for an unprivileged caller.
            let status = reply.words[0] as isize;
            if status != 0 {
                let _ = debug_print(&format!(
                    "{}: PASS denied status={}\n",
                    LABEL, status
                ));
                Ok(())
            } else {
                // Accepted: this is a security failure — unprivileged
                // caller obtained a cross-session proc listing.
                let _ = debug_print(&format!(
                    "{}: FAIL not denied (status=0)\n",
                    LABEL
                ));
                Err(())
            }
        }
        Err(e) => {
            // The kernel/transport propagated an error (e.g. PermissionDenied
            // raised before reply, or BadCap mapped to an IPC-level error).
            // That is *also* a deny outcome from the probe's perspective —
            // the cross-session aggregate did not flow back to us.
            let _ = debug_print(&format!(
                "{}: PASS denied ipc_err={:?}\n",
                LABEL, e
            ));
            Ok(())
        }
    }
}
