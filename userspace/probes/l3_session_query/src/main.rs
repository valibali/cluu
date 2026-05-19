//! Spec 3 acceptance marker: l3_session_query
//! Create session, query it, verify values returned.

#![no_std]
#![no_main]

extern crate alloc;
extern crate libcluu;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use libcluu::runtime as _;

use cluu_proto::session::{
    ProfileSpec, SessionCreateRequest, SessionState,
};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let label = "l3_session_query";

    let req = SessionCreateRequest {
        user_name: String::from("testuser"),
        profile: ProfileSpec {
            home: String::from("/home/testuser"),
            initial_view: cluu_proto::spawn::ViewSource::Derive(0xC0FFEE),
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
    libcluu::debug_print(&format!("{}: CREATE ok sid={}\n", label, ok.session_id));

    let reply = match libcluu::session::query(ok.token) {
        Ok(r) => r,
        Err(e) => {
            libcluu::debug_print(&format!("{}: QUERY failed {:?}\n", label, e));
            return 1;
        }
    };

    if reply.session_id != ok.session_id {
        libcluu::debug_print(&format!("{}: session_id mismatch {}\n", label, reply.session_id));
        return 1;
    }
    if reply.user_name != "testuser" {
        libcluu::debug_print(&format!("{}: user_name mismatch {}\n", label, reply.user_name));
        return 1;
    }
    if reply.state != SessionState::Live {
        libcluu::debug_print(&format!("{}: bad state {:?}\n", label, reply.state));
        return 1;
    }

    libcluu::debug_print(&format!(
        "{}: QUERY ok user={} leader={:?} members={:?}\n",
        label, reply.user_name, reply.leader_pid, reply.member_pids
    ));

    let _ = libcluu::session::destroy(ok.token);
    libcluu::debug_print(&format!("{}: PASS\n", label));
    0
}