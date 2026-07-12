//! Phase 13 probe: pm_cross_session_no_leak
//!
//! Verifies that capabilities derived from session A cannot impersonate session B.
//! Creates two distinct sessions, confirms session ids differ, confirms each
//! token's query reply reports its own session id (no cross-talk), and confirms
//! a narrowed derivative of A's token continues to report A's session id.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use libcluu::debug_print;
use cluu_wire::session::{
    ProfileSpec, SessionCreateRequest, RIGHT_SESSION_QUERY,
};
use cluu_wire::spawn::ViewSource;

const LABEL: &str = "pm_cross_session_no_leak";

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

fn make_req(user: &str, home: &str) -> SessionCreateRequest {
    SessionCreateRequest {
        user_name: String::from(user),
        password: String::new(),
        profile: ProfileSpec {
            home: String::from(home),
            initial_view: ViewSource::Derive(0xC0FFEE),
            env: vec![
                (String::from("USER"), String::from(user)),
                (String::from("HOME"), String::from(home)),
            ],
            umask: 0o022,
        },
    }
}

fn run() -> Result<(), ()> {
    let _ = debug_print(&format!("{}: start\n", LABEL));

    // 1. Create session A.
    let req_a = make_req("alice", "/tmp/alice");
    let a = match libcluu::session::create(req_a) {
        Ok(o) => o,
        Err(e) => {
            let _ = debug_print(&format!("{}: FAIL create A {:?}\n", LABEL, e));
            return Err(());
        }
    };
    let _ = debug_print(&format!(
        "{}: CREATE A ok session_id={}\n",
        LABEL, a.session_id
    ));

    // 2. Create session B.
    let req_b = make_req("guest", "/tmp/bob");
    let b = match libcluu::session::create(req_b) {
        Ok(o) => o,
        Err(e) => {
            let _ = debug_print(&format!("{}: FAIL create B {:?}\n", LABEL, e));
            let _ = libcluu::session::destroy(a.token);
            return Err(());
        }
    };
    let _ = debug_print(&format!(
        "{}: CREATE B ok session_id={}\n",
        LABEL, b.session_id
    ));

    // 3. Sids must differ.
    if a.session_id == b.session_id {
        let _ = debug_print(&format!(
            "{}: FAIL sids collide A={} B={}\n",
            LABEL, a.session_id, b.session_id
        ));
        let _ = libcluu::session::destroy(a.token);
        let _ = libcluu::session::destroy(b.token);
        return Err(());
    }

    // 4. Query A with A's token — must report A's sid.
    let qa = match libcluu::session::query(a.token) {
        Ok(r) => r,
        Err(e) => {
            let _ = debug_print(&format!("{}: FAIL query A {:?}\n", LABEL, e));
            let _ = libcluu::session::destroy(a.token);
            let _ = libcluu::session::destroy(b.token);
            return Err(());
        }
    };
    if qa.session_id != a.session_id {
        let _ = debug_print(&format!(
            "{}: FAIL query A reported sid={} expected {}\n",
            LABEL, qa.session_id, a.session_id
        ));
        let _ = libcluu::session::destroy(a.token);
        let _ = libcluu::session::destroy(b.token);
        return Err(());
    }
    if qa.user_name != "alice" {
        let _ = debug_print(&format!(
            "{}: FAIL query A reported user={} expected alice\n",
            LABEL, qa.user_name
        ));
        let _ = libcluu::session::destroy(a.token);
        let _ = libcluu::session::destroy(b.token);
        return Err(());
    }

    // 5. Query B with B's token — must report B's sid, not A's.
    let qb = match libcluu::session::query(b.token) {
        Ok(r) => r,
        Err(e) => {
            let _ = debug_print(&format!("{}: FAIL query B {:?}\n", LABEL, e));
            let _ = libcluu::session::destroy(a.token);
            let _ = libcluu::session::destroy(b.token);
            return Err(());
        }
    };
    if qb.session_id != b.session_id {
        let _ = debug_print(&format!(
            "{}: FAIL query B reported sid={} expected {}\n",
            LABEL, qb.session_id, b.session_id
        ));
        let _ = libcluu::session::destroy(a.token);
        let _ = libcluu::session::destroy(b.token);
        return Err(());
    }
    if qb.session_id == a.session_id {
        let _ = debug_print(&format!(
            "{}: FAIL query B leaked A's sid={}\n",
            LABEL, a.session_id
        ));
        let _ = libcluu::session::destroy(a.token);
        let _ = libcluu::session::destroy(b.token);
        return Err(());
    }
    if qb.user_name != "bob" {
        let _ = debug_print(&format!(
            "{}: FAIL query B reported user={} expected bob\n",
            LABEL, qb.user_name
        ));
        let _ = libcluu::session::destroy(a.token);
        let _ = libcluu::session::destroy(b.token);
        return Err(());
    }

    // 6. Narrow A's token to QUERY-only and confirm it still reports A's sid,
    //    never crossing over into B's identity.
    let a_narrow = match libcluu::session::derive_token(a.token, RIGHT_SESSION_QUERY) {
        Ok(t) => t,
        Err(e) => {
            let _ = debug_print(&format!("{}: FAIL derive narrow A {:?}\n", LABEL, e));
            let _ = libcluu::session::destroy(a.token);
            let _ = libcluu::session::destroy(b.token);
            return Err(());
        }
    };
    let qa_narrow = match libcluu::session::query(a_narrow) {
        Ok(r) => r,
        Err(e) => {
            let _ = debug_print(&format!(
                "{}: FAIL query narrow A {:?}\n",
                LABEL, e
            ));
            let _ = libcluu::session::destroy(a.token);
            let _ = libcluu::session::destroy(b.token);
            return Err(());
        }
    };
    if qa_narrow.session_id != a.session_id {
        let _ = debug_print(&format!(
            "{}: FAIL narrow A leak sid={} expected {}\n",
            LABEL, qa_narrow.session_id, a.session_id
        ));
        let _ = libcluu::session::destroy(a.token);
        let _ = libcluu::session::destroy(b.token);
        return Err(());
    }
    if qa_narrow.session_id == b.session_id {
        let _ = debug_print(&format!(
            "{}: FAIL narrow A bled into B sid={}\n",
            LABEL, b.session_id
        ));
        let _ = libcluu::session::destroy(a.token);
        let _ = libcluu::session::destroy(b.token);
        return Err(());
    }

    // Repeat the query a couple more times to make sure sids stay stable.
    for i in 0..3 {
        let r = match libcluu::session::query(a.token) {
            Ok(r) => r,
            Err(e) => {
                let _ = debug_print(&format!(
                    "{}: FAIL repeat A {} {:?}\n",
                    LABEL, i, e
                ));
                let _ = libcluu::session::destroy(a.token);
                let _ = libcluu::session::destroy(b.token);
                return Err(());
            }
        };
        if r.session_id != a.session_id {
            let _ = debug_print(&format!(
                "{}: FAIL repeat A {} sid={} expected {}\n",
                LABEL, i, r.session_id, a.session_id
            ));
            let _ = libcluu::session::destroy(a.token);
            let _ = libcluu::session::destroy(b.token);
            return Err(());
        }
        let r = match libcluu::session::query(b.token) {
            Ok(r) => r,
            Err(e) => {
                let _ = debug_print(&format!(
                    "{}: FAIL repeat B {} {:?}\n",
                    LABEL, i, e
                ));
                let _ = libcluu::session::destroy(a.token);
                let _ = libcluu::session::destroy(b.token);
                return Err(());
            }
        };
        if r.session_id != b.session_id {
            let _ = debug_print(&format!(
                "{}: FAIL repeat B {} sid={} expected {}\n",
                LABEL, i, r.session_id, b.session_id
            ));
            let _ = libcluu::session::destroy(a.token);
            let _ = libcluu::session::destroy(b.token);
            return Err(());
        }
    }

    // 7. Cleanup — destroy both.
    if let Err(e) = libcluu::session::destroy(a.token) {
        let _ = debug_print(&format!("{}: FAIL destroy A {:?}\n", LABEL, e));
        let _ = libcluu::session::destroy(b.token);
        return Err(());
    }
    if let Err(e) = libcluu::session::destroy(b.token) {
        let _ = debug_print(&format!("{}: FAIL destroy B {:?}\n", LABEL, e));
        return Err(());
    }

    let _ = debug_print(&format!(
        "{}: PASS sidA={} sidB={}\n",
        LABEL, a.session_id, b.session_id
    ));
    Ok(())
}
