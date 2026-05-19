//! Spec 2 §12 acceptance marker: verify SIGINT delivery via raise()
//!
//! Installs a SIGINT handler, calls raise(SIGINT), verifies handler was invoked.
//! NOTE: tests local signal path (raise → handler in-process), not the
//! end-to-end line-discipline → procmgr → process path, because there is no
//! PTS input-injection API available in-tree. Follow-up: spec 3 may add a
//! probe injection verb.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use core::sync::atomic::{AtomicBool, Ordering};

static GOT_SIGNAL: AtomicBool = AtomicBool::new(false);

extern "C" fn handler(_sig: i32) {
    GOT_SIGNAL.store(true, Ordering::SeqCst);
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let marker = b"l2_sigint_delivered";
    // Install handler via libcluu POSIX signal()
    let sig: libcluu::posix::c_int = 2; // SIGINT
    libcluu::posix::signal::signal(sig, handler as libcluu::posix::signal::sighandler_t);

    // Self-trigger
    libcluu::posix::signal::raise(sig);

    if GOT_SIGNAL.load(Ordering::SeqCst) {
        let _ = libcluu::debug_print(&format!("{}: PASS\n", core::str::from_utf8(marker).unwrap_or("?")));
        0
    } else {
        let _ = libcluu::debug_print(&format!("{}: FAIL (handler not invoked)\n", core::str::from_utf8(marker).unwrap_or("?")));
        1
    }
}