//! Negative test: container without NET profile cannot reach netd.
//!
//! This probe has PROFILE ipc vfs (no net). TOKEN_EXTRA_0 is 0 because
//! procmgr's derive_netd_token returns None for non-NET profiles.
//! has_netd() must return false — the netd endpoint token is
//! structurally absent from the spawn envelope (AGENTS.md §3: no
//! runtime ACL).
//!
//! We use has_netd() rather than socket() because socket() calls
//! set_errno() → pthread_self() which reads FS:8 (TLS).  If init_tls()
//! silently failed for this probe, FS base is 0 and the TLS read
//! page-faults.  has_netd() checks the same invariant (TOKEN_EXTRA_0
//! absent) via process_info() which needs no TLS.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::posix::socket;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if !socket::has_netd() {
        let _ = libcluu::debug_print("NET_CAP_NEGATIVE_OK\n");
        0
    } else {
        let _ = libcluu::debug_print("NET_CAP_NEGATIVE_FAIL: netd token present without NET profile\n");
        1
    }
}
