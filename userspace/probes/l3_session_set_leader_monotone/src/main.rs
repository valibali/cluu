//! Spec 3 acceptance marker: l3_session_set_leader_monotone
//! Create session, set leader, verify set-once (second set fails).

#![no_std]
#![no_main]

extern crate alloc;
extern crate libcluu;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use libcluu::runtime as _;

use cluu_wire::session::{
    ProfileSpec, SessionCreateRequest, SessionErr,
};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let label = "l3_session_set_leader_monotone";

    let req = SessionCreateRequest {
        user_name: String::from("testuser"),
        profile: ProfileSpec {
            home: String::from("/home/testuser"),
            initial_view: cluu_wire::spawn::ViewSource::Derive(0xC0FFEE),
            env: vec![(String::from("USER"), String::from("testuser"))],
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

    let my_pid = libcluu::boot::pid() as u32;

    let result = libcluu::session::set_leader(ok.token, my_pid);
    libcluu::debug_print(&format!("{}: first SET_LEADER({}) = {:?}\n", label, my_pid, result));
    if result.is_err() {
        libcluu::debug_print(&format!("{}: first SET_LEADER failed {:?}\n", label, result.err()));
        let _ = libcluu::session::destroy(ok.token);
        return 1;
    }

    let second = libcluu::session::set_leader(ok.token, my_pid + 1);
    libcluu::debug_print(&format!("{}: second SET_LEADER({}) = {:?}\n", label, my_pid + 1, second));
    match second {
        Ok(()) => {
            libcluu::debug_print(&format!("{}: second SET_LEADER should have failed\n", label));
            let _ = libcluu::session::destroy(ok.token);
            return 1;
        }
        Err(SessionErr::AlreadyHasLeader) => {
            libcluu::debug_print(&format!("{}: second SET_LEADER correctly denied\n", label));
        }
        Err(e) => {
            libcluu::debug_print(&format!("{}: second SET_LEADER wrong error {:?}\n", label, e));
            let _ = libcluu::session::destroy(ok.token);
            return 1;
        }
    }

    let _ = libcluu::session::destroy(ok.token);
    libcluu::debug_print(&format!("{}: PASS\n", label));
    0
}