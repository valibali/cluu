//! Spec 3 acceptance marker: l3_getty_auth_spawns_shell
//! Test getty auth flow — accepts all credentials, verify shell spawns.

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
use cluu_wire::spawn::{SpawnEnvelope, ViewSource};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let label = "l3_getty_auth_spawns_shell";

    // This probe simulates what getty does after auth:
    // 1. Create a session for the user.
    // 2. Spawn the shell in that session.

    let req = SessionCreateRequest {
        user_name: String::from("testuser"),
        profile: ProfileSpec {
            home: String::from("/home/testuser"),
            initial_view: ViewSource::Derive(0xC0FFEE),
            env: vec![
                (String::from("USER"), String::from("testuser")),
                (String::from("HOME"), String::from("/home/testuser")),
                (String::from("TERM"), String::from("vt100")),
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

    // Spawn a shell.
    let envelope = SpawnEnvelope {
        image: String::from("shell"),
        args: vec![],
        env: vec![
            (String::from("TERM"), String::from("vt100")),
            (String::from("USER"), String::from("testuser")),
            (String::from("HOME"), String::from("/home/testuser")),
        ],
        view: ViewSource::Derive(0xC0FFEE),
        fd_inherit: vec![],
        session: Some(ok.token),
        notify: None,
    };

    let spawn_result = libcluu::spawn::spawn(envelope);
    match spawn_result {
        Ok(reply) => {
            libcluu::debug_print(&format!("{}: SPAWN shell ok pid={}\n", label, reply.pid));
        }
        Err(e) => {
            libcluu::debug_print(&format!("{}: SPAWN shell failed {:?}\n", label, e));
            let _ = libcluu::session::destroy(ok.token);
            return 1;
        }
    }

    let _ = libcluu::session::destroy(ok.token);
    libcluu::debug_print(&format!("{}: PASS\n", label));
    0
}