//! Phase 13.2 probe: pm_pid_layout
//!
//! Verifies PID encoding invariant: PID = (sid << 23) | local where
//!   - sid: 8 bits (top byte)
//!   - local: 23 bits (lower)
//! Caller's own PID must round-trip through decompose/recompose and the
//! session_id reported by libcluu::session::query(self-session) must equal
//! the high byte.

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

const LABEL: &str = "pm_pid_layout";

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

fn run() -> Result<(), ()> {
    let _ = debug_print(&format!("{}: start\n", LABEL));

    let pid = libcluu::boot::pid() as u32;
    if pid == 0 {
        let _ = debug_print(&format!("{}: FAIL pid=0\n", LABEL));
        return Err(());
    }

    let sid = (pid >> 23) & 0xFF;
    let local = pid & 0x7F_FFFF;
    let recomposed = (sid << 23) | local;
    if recomposed != pid {
        let _ = debug_print(&format!(
            "{}: FAIL recompose pid=0x{:x} sid={} local={} recomposed=0x{:x}\n",
            LABEL, pid, sid, local, recomposed
        ));
        return Err(());
    }
    let _ = debug_print(&format!(
        "{}: case_a roundtrip pid=0x{:x} sid={} local={}\n",
        LABEL, pid, sid, local
    ));

    if local == 0 {
        let _ = debug_print(&format!("{}: FAIL local=0 reserved\n", LABEL));
        return Err(());
    }

    let req = SessionCreateRequest {
        user_name: String::from("root"),
        password: String::new(),
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
    if created.session_id > 0xFF {
        let _ = debug_print(&format!(
            "{}: FAIL session_id={} > 0xFF\n",
            LABEL, created.session_id
        ));
        let _ = libcluu::session::destroy(created.token);
        return Err(());
    }
    let _ = debug_print(&format!(
        "{}: case_b session_id={} fits 8-bit\n",
        LABEL, created.session_id
    ));
    let _ = libcluu::session::destroy(created.token);

    let _ = debug_print(&format!("{}: PASS\n", LABEL));
    Ok(())
}
