//! procmgr-cap-refactor probe: pm_bootstrap_two_pmgr
//!
//! Verifies the two-procmgr bootstrap is wired correctly:
//!   1. root-procmgr is registered as `procmgr:spawn`.
//!   2. Creating a new session triggers root-procmgr to spawn a
//!      session-procmgr that registers as `session-procmgr:spawn:<sid>`.
//!   3. Both registry lookups return Some(endpoint).
//!   4. The two endpoint tokens are DIFFERENT handles (no aliasing).
//!
//! Validates that root-procmgr and session-procmgr are two distinct
//! services with disjoint authority, not the same process behind two
//! registry names.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use libcluu::debug_print;
use cluu_wire::session::{ProfileSpec, SessionCreateRequest};
use cluu_wire::spawn::ViewSource;

const LABEL: &str = "pm_bootstrap_two_pmgr";

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

fn run() -> Result<(), ()> {
    let _ = debug_print(&format!("{}: start\n", LABEL));

    // 1. Look up root-procmgr.
    let root_name = "procmgr:spawn";
    let root_ep = match libcluu::registry::lookup_service(root_name) {
        Some(t) => t,
        None => {
            let _ = debug_print(&format!(
                "{}: FAIL root-procmgr not found ({})\n",
                LABEL, root_name
            ));
            return Err(());
        }
    };
    let _ = debug_print(&format!(
        "{}: root-procmgr ep={}\n",
        LABEL, root_ep
    ));

    // 2. Create a session — root-procmgr spawns a session-procmgr for it.
    let req = SessionCreateRequest {
        user_name: String::from("bootstrap"),
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
            let _ = debug_print(&format!("{}: FAIL session create {:?}\n", LABEL, e));
            return Err(());
        }
    };
    let _ = debug_print(&format!(
        "{}: CREATE ok session_id={}\n",
        LABEL, created.session_id
    ));

    // 3. Look up session-procmgr for the freshly created session.
    let session_name = format!("session-procmgr:spawn:{}", created.session_id);
    let session_ep = match libcluu::registry::lookup_service(&session_name) {
        Some(t) => t,
        None => {
            let _ = debug_print(&format!(
                "{}: FAIL session-procmgr not found ({})\n",
                LABEL, session_name
            ));
            let _ = libcluu::session::destroy(created.token);
            return Err(());
        }
    };
    let _ = debug_print(&format!(
        "{}: session-procmgr ep={}\n",
        LABEL, session_ep
    ));

    // 4. Endpoints must differ.
    if root_ep == session_ep {
        let _ = debug_print(&format!(
            "{}: FAIL endpoints aliased ep={}\n",
            LABEL, root_ep
        ));
        let _ = libcluu::session::destroy(created.token);
        return Err(());
    }

    let _ = debug_print(&format!(
        "{}: PASS root_ep={} session_ep={} sid={}\n",
        LABEL, root_ep, session_ep, created.session_id
    ));
    let _ = libcluu::session::destroy(created.token);
    Ok(())
}
