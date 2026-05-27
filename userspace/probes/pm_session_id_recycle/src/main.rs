//! Probe: pm_session_id_recycle
//!
//! Verifies session_id allocation behaviour after a destroy in the middle of
//! a sequence of creates. Either outcome is acceptable:
//!   (a) recycle  — the freed sid is handed out to the next create, or
//!   (b) monotone — sids are issued strictly increasing and the freed sid
//!                   stays vacant.
//!
//! Steps:
//!   1. Create 3 sessions, record sid_1 / sid_2 / sid_3.
//!   2. Destroy the middle session (#2).
//!   3. Create one more session, record sid_4.
//!   4. Classify the run as recycle (sid_4 == sid_2) or monotone
//!      (sid_4 > sid_3). Any other shape FAILs.
//!   5. Destroy all remaining sessions.

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
use cluu_wire::TokenHandle;
use libcluu::debug_print;

const LABEL: &str = "pm_session_id_recycle";

fn make_request(user: &str) -> SessionCreateRequest {
    SessionCreateRequest {
        user_name: String::from(user),
        profile: ProfileSpec {
            home: String::from("/tmp"),
            initial_view: ViewSource::Derive(0xC0FFEE),
            env: vec![],
            umask: 0o022,
        },
    }
}

fn create_one(label_user: &str) -> Result<(TokenHandle, u32), ()> {
    let req = make_request(label_user);
    match libcluu::session::create(req) {
        Ok(ok) => {
            let _ = debug_print(&format!(
                "{}: CREATE ok user={} session_id={}\n",
                LABEL, label_user, ok.session_id
            ));
            Ok((ok.token, ok.session_id))
        }
        Err(e) => {
            let _ = debug_print(&format!(
                "{}: FAIL CREATE user={} err={:?}\n",
                LABEL, label_user, e
            ));
            Err(())
        }
    }
}

fn destroy_one(token: TokenHandle, tag: &str) -> Result<(), ()> {
    match libcluu::session::destroy(token) {
        Ok(()) => {
            let _ = debug_print(&format!("{}: DESTROY ok tag={}\n", LABEL, tag));
            Ok(())
        }
        Err(e) => {
            let _ = debug_print(&format!(
                "{}: FAIL DESTROY tag={} err={:?}\n",
                LABEL, tag, e
            ));
            Err(())
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

fn run() -> Result<(), ()> {
    let _ = debug_print(&format!("{}: start\n", LABEL));

    // Step 1: create three sessions.
    let (tok1, sid1) = create_one("recycle_a")?;
    let (tok2, sid2) = match create_one("recycle_b") {
        Ok(v) => v,
        Err(()) => {
            let _ = destroy_one(tok1, "tok1_cleanup_on_create2_fail");
            return Err(());
        }
    };
    let (tok3, sid3) = match create_one("recycle_c") {
        Ok(v) => v,
        Err(()) => {
            let _ = destroy_one(tok2, "tok2_cleanup_on_create3_fail");
            let _ = destroy_one(tok1, "tok1_cleanup_on_create3_fail");
            return Err(());
        }
    };

    let _ = debug_print(&format!(
        "{}: initial sids sid1={} sid2={} sid3={}\n",
        LABEL, sid1, sid2, sid3
    ));

    // Sanity: the three initial sids must be distinct.
    if sid1 == sid2 || sid2 == sid3 || sid1 == sid3 {
        let _ = debug_print(&format!(
            "{}: FAIL initial sids not distinct sid1={} sid2={} sid3={}\n",
            LABEL, sid1, sid2, sid3
        ));
        let _ = destroy_one(tok3, "tok3");
        let _ = destroy_one(tok2, "tok2");
        let _ = destroy_one(tok1, "tok1");
        return Err(());
    }

    // Step 2: destroy the middle one.
    if destroy_one(tok2, "middle_sid2").is_err() {
        let _ = destroy_one(tok3, "tok3");
        let _ = destroy_one(tok1, "tok1");
        return Err(());
    }

    // Step 3: create a fourth session and capture its sid.
    let (tok4, sid4) = match create_one("recycle_d") {
        Ok(v) => v,
        Err(()) => {
            let _ = destroy_one(tok3, "tok3");
            let _ = destroy_one(tok1, "tok1");
            return Err(());
        }
    };
    let _ = debug_print(&format!("{}: post-destroy sid4={}\n", LABEL, sid4));

    // Step 4: classify.
    let result: Result<&'static str, ()> = if sid4 == sid2 {
        Ok("recycle")
    } else if sid4 > sid3 {
        Ok("monotone")
    } else {
        let _ = debug_print(&format!(
            "{}: FAIL unexpected sid4={} (sid1={} sid2={} sid3={})\n",
            LABEL, sid4, sid1, sid2, sid3
        ));
        Err(())
    };

    // Step 5: destroy everything still alive.
    let _ = destroy_one(tok4, "tok4");
    let _ = destroy_one(tok3, "tok3");
    let _ = destroy_one(tok1, "tok1");

    match result {
        Ok(kind) => {
            let _ = debug_print(&format!(
                "{}: PASS {} sid1={} sid2={} sid3={} sid4={}\n",
                LABEL, kind, sid1, sid2, sid3, sid4
            ));
            Ok(())
        }
        Err(()) => Err(()),
    }
}
