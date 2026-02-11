//! Minimal signal/sigaction compatibility.
//!
//! This is a userspace compatibility layer, not full POSIX signal delivery.
//! - `signal`/`sigaction` store handlers locally.
//! - `raise` invokes the local handler for the current thread/process context.
//! - default action falls back to `_kill(getpid(), sig)` so procmgr policy applies.

use super::{c_int, c_ulong};
use crate::errno::{set_errno, EINVAL};
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

pub type sighandler_t = usize;

pub const NSIG: usize = 64;
pub const SIG_DFL: sighandler_t = 0;
pub const SIG_IGN: sighandler_t = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct sigaction {
    pub sa_handler: sighandler_t,
    pub sa_flags: c_int,
    pub sa_mask: c_ulong,
}

struct SignalState {
    handlers: Vec<sighandler_t>,
}

impl SignalState {
    fn new() -> Self {
        Self {
            handlers: vec![SIG_DFL; NSIG],
        }
    }

    fn valid_signal(sig: c_int) -> bool {
        sig > 0 && (sig as usize) < NSIG
    }
}

static SIGNAL_STATE: Mutex<Option<SignalState>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut SignalState) -> R) -> R {
    let mut guard = SIGNAL_STATE.lock();
    let state = guard.get_or_insert_with(SignalState::new);
    f(state)
}

#[no_mangle]
pub extern "C" fn signal(sig: c_int, handler: sighandler_t) -> sighandler_t {
    if !SignalState::valid_signal(sig) {
        set_errno(EINVAL);
        return usize::MAX;
    }

    with_state(|state| {
        let idx = sig as usize;
        let old = state.handlers[idx];
        state.handlers[idx] = handler;
        old
    })
}

#[no_mangle]
pub extern "C" fn sigaction(sig: c_int, act: *const sigaction, oldact: *mut sigaction) -> c_int {
    if !SignalState::valid_signal(sig) {
        set_errno(EINVAL);
        return -1;
    }

    with_state(|state| {
        let idx = sig as usize;
        let old = sigaction {
            sa_handler: state.handlers[idx],
            sa_flags: 0,
            sa_mask: 0,
        };

        if !oldact.is_null() {
            unsafe {
                *oldact = old;
            }
        }

        if !act.is_null() {
            let new_act = unsafe { *act };
            state.handlers[idx] = new_act.sa_handler;
        }
    });

    0
}

#[no_mangle]
pub extern "C" fn raise(sig: c_int) -> c_int {
    if !SignalState::valid_signal(sig) {
        set_errno(EINVAL);
        return -1;
    }

    let handler = with_state(|state| state.handlers[sig as usize]);
    if handler == SIG_IGN {
        return 0;
    }
    if handler == SIG_DFL {
        return super::_kill(super::_getpid(), sig);
    }

    // Call user handler directly in current context.
    let func: extern "C" fn(c_int) = unsafe { core::mem::transmute(handler) };
    func(sig);
    0
}
