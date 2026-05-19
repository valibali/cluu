//! Spec 3 acceptance marker: l3_session_derive_narrow
//! Create session, derive token with subset rights, verify rights cap works.

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
    RIGHT_SESSION_CONTROL, RIGHT_SESSION_QUERY,
};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let label = "l3_session_derive_narrow";

    // Create session.
    let req = SessionCreateRequest {
        user_name: String::from("testuser"),
        profile: ProfileSpec {
            home: String::from("/home/testuser"),
            initial_view: cluu_wire::spawn::ViewSource::Derive(0xC0FFEE),
            env: vec![
                (String::from("USER"), String::from("testuser")),
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
    libcluu::debug_print(&format!("{}: CREATE ok\n", label));

    // Derive a QUERY-only token.
    let narrow = match libcluu::session::derive_token(ok.token, RIGHT_SESSION_QUERY) {
        Ok(t) => t,
        Err(e) => {
            libcluu::debug_print(&format!("{}: DERIVE failed {:?}\n", label, e));
            return 1;
        }
    };
    libcluu::debug_print(&format!("{}: DERIVE ok\n", label));

    // QUERY works with narrow token.
    match libcluu::session::query(narrow) {
        Ok(_) => {
            libcluu::debug_print(&format!("{}: QUERY with narrow token OK\n", label));
        }
        Err(e) => {
            libcluu::debug_print(&format!("{}: QUERY with narrow token FAILED {:?}\n", label, e));
            return 1;
        }
    }

    // CONTROL (destroy) must fail with QUERY-only token.
    match libcluu::session::destroy(narrow) {
        Ok(()) => {
            libcluu::debug_print(&format!("{}: DESTROY with narrow token SHOULD HAVE FAILED\n", label));
            return 1;
        }
        Err(e) => {
            libcluu::debug_print(&format!("{}: DESTROY with narrow correctly denied {:?}\n", label, e));
        }
    }

    // Clean up with the full-rights token.
    let _ = libcluu::session::destroy(ok.token);
    libcluu::debug_print(&format!("{}: PASS\n", label));
    0
}