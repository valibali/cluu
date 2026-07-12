//! Spec 2 §12 acceptance marker: service death sends SIGHUP / POLLHUP
//!
//! DEFERRED: requires multi-process orchestration (kill cluuterm service while
//! probe holds an open fd, verify POLLHUP or SIGHUP received). Spec 3 will
//! add the probe infrastructure needed to coordinate service lifecycle with
//! a marker process.
//!
//! Stub: always reports DEFERRED.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;


#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = libcluu::debug_print("l2_pts_service_death_hangup: DEFERRED (needs service lifecycle coordination from spec 3)\n");
    0
}