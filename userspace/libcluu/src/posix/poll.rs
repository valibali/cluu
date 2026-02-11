//! Minimal poll(2) compatibility layer.
//!
//! This implementation is intentionally conservative:
//! - Regular seekable files are treated as readable.
//! - Writable descriptors are treated as writable.
//! - TTY readability is not speculatively consumed/peeked here.

use super::c_int;
use crate::errno::{set_errno, EINVAL};
use crate::fd_table::FD_TABLE;

pub type nfds_t = usize;

pub const POLLIN: i16 = 0x0001;
pub const POLLOUT: i16 = 0x0004;
pub const POLLERR: i16 = 0x0008;
pub const POLLNVAL: i16 = 0x0020;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct pollfd {
    pub fd: c_int,
    pub events: i16,
    pub revents: i16,
}

#[no_mangle]
pub extern "C" fn poll(fds: *mut pollfd, nfds: nfds_t, timeout: c_int) -> c_int {
    if timeout < -1 {
        set_errno(EINVAL);
        return -1;
    }
    if nfds > 0 && fds.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let mut remaining_ms = timeout;
    loop {
        let mut ready: c_int = 0;

        if nfds > 0 {
            let entries = unsafe { core::slice::from_raw_parts_mut(fds, nfds) };
            let table = FD_TABLE.lock();
            for pfd in entries.iter_mut() {
                pfd.revents = 0;

                // poll ignores negative fds
                if pfd.fd < 0 {
                    continue;
                }

                let entry = match table.get(pfd.fd) {
                    Some(e) => e,
                    None => {
                        pfd.revents |= POLLNVAL;
                        ready += 1;
                        continue;
                    }
                };

                if (pfd.events & POLLIN) != 0 {
                    // Conservative model: regular seekable file descriptors are readable.
                    if entry.is_seekable() {
                        pfd.revents |= POLLIN;
                    }
                }
                if (pfd.events & POLLOUT) != 0 && entry.is_writable() {
                    pfd.revents |= POLLOUT;
                }
                if pfd.revents != 0 {
                    ready += 1;
                }
            }
        }

        if ready > 0 {
            return ready;
        }
        if timeout == 0 {
            return 0;
        }
        if timeout > 0 {
            if remaining_ms <= 0 {
                return 0;
            }
            remaining_ms -= 1;
        }

        // 1ms scheduler-friendly wait.
        let _ = super::time::usleep(1000);
    }
}

#[no_mangle]
pub extern "C" fn _poll_r(
    _r: *mut core::ffi::c_void,
    fds: *mut pollfd,
    nfds: nfds_t,
    timeout: c_int,
) -> c_int {
    poll(fds, nfds, timeout)
}
