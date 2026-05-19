//! Spec 3 acceptance marker: l3_session_create_destroy
//! Simple smoke test: create session, verify it exists, destroy it.

#![no_std]
#![no_main]

extern crate alloc;
extern crate libcluu;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use libcluu::runtime as _;

use cluu_wire::session::{
    ProfileSpec, SessionCreateRequest,
};
use cluu_wire::spawn::ViewSource;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let label = "l3_session_create_destroy";

    // Create a session with minimal profile.
    let req = SessionCreateRequest {
        user_name: String::from("testuser"),
        profile: ProfileSpec {
            home: String::from("/home/testuser"),
            initial_view: ViewSource::Derive(0xC0FFEE),
            env: vec![
                (String::from("USER"), String::from("testuser")),
                (String::from("HOME"), String::from("/home/testuser")),
            ],
            umask: 0o022,
        },
    };

    let ok = match libcluu::session::create(req) {
        Ok(o) => o,
        Err(e) => {
            libcluu::debug_print(&format!("{}: CREATE failed {:?}\n", label, e));
            return 1;
        }
    };
    libcluu::debug_print(&format!("{}: CREATE ok session_id={}\n", label, ok.session_id));

    // Query — verify it's alive.
    match libcluu::session::query(ok.token) {
        Ok(reply) => {
            libcluu::debug_print(&format!(
                "{}: QUERY ok session_id={} user={} state={:?}\n",
                label, reply.session_id, reply.user_name, reply.state
            ));
        }
        Err(e) => {
            libcluu::debug_print(&format!("{}: QUERY failed {:?}\n", label, e));
            return 1;
        }
    }

    // Destroy.
    match libcluu::session::destroy(ok.token) {
        Ok(()) => {
            libcluu::debug_print(&format!("{}: DESTROY ok\n", label));
        }
        Err(e) => {
            libcluu::debug_print(&format!("{}: DESTROY failed {:?}\n", label, e));
            return 1;
        }
    }

    libcluu::debug_print(&format!("{}: PASS\n", label));
    0
}