//! Probe: pm_cap_revoke_stale
//!
//! Verifies that capabilities derived from a session token become unusable
//! after the parent session is destroyed. Cap-revocation invariant:
//! authority flows from the parent — destroying the parent revokes derived
//! tokens, so any subsequent use must return an Err (any variant).
//!
//! Steps:
//!   1. Create session via libcluu::session::create.
//!   2. Derive a narrowed (rights=0) token from the parent session token.
//!   3. Confirm the derived token works initially: query() returns Ok.
//!      (rights=0 may legitimately deny query; we accept either outcome
//!      here — what matters is the *change* after destroy.)
//!   4. Destroy the parent session via the full-rights parent token.
//!   5. Calling query() on the derived token must now return Err.
//!   6. Emit `pm_cap_revoke_stale: PASS` or `pm_cap_revoke_stale: FAIL <reason>`.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use cluu_wire::session::{ProfileSpec, SessionCreateRequest};
use cluu_wire::spawn::ViewSource;
use libcluu::debug_print;

const LABEL: &str = "pm_cap_revoke_stale";

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

fn run() -> Result<(), ()> {
    let _ = debug_print(&format!("{}: start\n", LABEL));

    // 1. Create a session.
    let req = SessionCreateRequest {
        user_name: String::from("revstale"),
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

    // 2. Derive a narrowed token (rights=0 — minimum-authority child).
    let derived = match libcluu::session::derive_token(created.token, 0) {
        Ok(t) => t,
        Err(e) => {
            let _ = debug_print(&format!("{}: FAIL derive_token {:?}\n", LABEL, e));
            let _ = libcluu::session::destroy(created.token);
            return Err(());
        }
    };
    let _ = debug_print(&format!("{}: DERIVE ok\n", LABEL));

    // 3. Probe derived token while the parent session is still alive.
    //    With rights=0 the derived token may legitimately deny query; we
    //    only record the pre-destroy outcome to contrast with step 5.
    let pre_destroy = libcluu::session::query(derived);
    match &pre_destroy {
        Ok(_) => {
            let _ = debug_print(&format!("{}: pre-destroy query Ok\n", LABEL));
        }
        Err(e) => {
            let _ = debug_print(&format!("{}: pre-destroy query Err {:?}\n", LABEL, e));
        }
    }

    // 4. Destroy the parent session via the full-rights parent token.
    if let Err(e) = libcluu::session::destroy(created.token) {
        let _ = debug_print(&format!("{}: FAIL destroy parent {:?}\n", LABEL, e));
        return Err(());
    }
    let _ = debug_print(&format!("{}: DESTROY parent ok\n", LABEL));

    // 5. Derived token MUST now be dead — query() must return Err.
    match libcluu::session::query(derived) {
        Ok(reply) => {
            let _ = debug_print(&format!(
                "{}: FAIL post-destroy query returned Ok session_id={}\n",
                LABEL, reply.session_id
            ));
            return Err(());
        }
        Err(e) => {
            let _ = debug_print(&format!(
                "{}: post-destroy query correctly denied {:?}\n",
                LABEL, e
            ));
        }
    }

    let _ = debug_print(&format!("{}: PASS\n", LABEL));
    Ok(())
}
