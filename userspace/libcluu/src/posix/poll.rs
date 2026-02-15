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

// ═══════════════════════════════════════════════════════════════════════════
// select(2) compatibility
// ═══════════════════════════════════════════════════════════════════════════

/// Maximum fd number supported by fd_set (1024, matching Linux FD_SETSIZE).
const FD_SETSIZE: usize = 1024;
/// Number of bits per long (64 on x86_64).
const NFDBITS: usize = 64;
/// Number of longs in an fd_set.
const FD_SET_LONGS: usize = FD_SETSIZE / NFDBITS;

/// fd_set bitmask for select().
#[repr(C)]
#[derive(Clone, Copy)]
pub struct fd_set {
    fds_bits: [u64; FD_SET_LONGS],
}

#[no_mangle]
pub extern "C" fn __cluu_fd_zero(set: *mut fd_set) {
    if !set.is_null() {
        unsafe {
            (*set).fds_bits = [0; FD_SET_LONGS];
        }
    }
}

#[no_mangle]
pub extern "C" fn __cluu_fd_set(fd: c_int, set: *mut fd_set) {
    if !set.is_null() && fd >= 0 && (fd as usize) < FD_SETSIZE {
        let idx = fd as usize / NFDBITS;
        let bit = fd as usize % NFDBITS;
        unsafe {
            (*set).fds_bits[idx] |= 1u64 << bit;
        }
    }
}

#[no_mangle]
pub extern "C" fn __cluu_fd_clr(fd: c_int, set: *mut fd_set) {
    if !set.is_null() && fd >= 0 && (fd as usize) < FD_SETSIZE {
        let idx = fd as usize / NFDBITS;
        let bit = fd as usize % NFDBITS;
        unsafe {
            (*set).fds_bits[idx] &= !(1u64 << bit);
        }
    }
}

#[no_mangle]
pub extern "C" fn __cluu_fd_isset(fd: c_int, set: *const fd_set) -> c_int {
    if set.is_null() || fd < 0 || (fd as usize) >= FD_SETSIZE {
        return 0;
    }
    let idx = fd as usize / NFDBITS;
    let bit = fd as usize % NFDBITS;
    if unsafe { (*set).fds_bits[idx] } & (1u64 << bit) != 0 {
        1
    } else {
        0
    }
}

/// timeval for select() timeout.
#[repr(C)]
pub struct timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// POSIX select(2).
///
/// Converts fd_set bitmasks to pollfd array, calls poll(), converts back.
#[no_mangle]
pub extern "C" fn select(
    nfds: c_int,
    readfds: *mut fd_set,
    writefds: *mut fd_set,
    exceptfds: *mut fd_set,
    timeout: *mut timeval,
) -> c_int {
    if nfds < 0 || nfds as usize > FD_SETSIZE {
        set_errno(EINVAL);
        return -1;
    }

    // Convert timeout to milliseconds for poll()
    let timeout_ms: c_int = if timeout.is_null() {
        -1 // block indefinitely
    } else {
        let tv = unsafe { &*timeout };
        if tv.tv_sec < 0 || tv.tv_usec < 0 {
            set_errno(EINVAL);
            return -1;
        }
        let ms = tv.tv_sec.saturating_mul(1000) + tv.tv_usec / 1000;
        if ms > i32::MAX as i64 {
            i32::MAX
        } else {
            ms as c_int
        }
    };

    // Build pollfd array from fd_sets
    let mut poll_fds: [pollfd; 64] = [pollfd {
        fd: -1,
        events: 0,
        revents: 0,
    }; 64];
    let mut npoll = 0usize;

    for fd in 0..nfds {
        let mut events: i16 = 0;

        if !readfds.is_null() && __cluu_fd_isset(fd, readfds) != 0 {
            events |= POLLIN;
        }
        if !writefds.is_null() && __cluu_fd_isset(fd, writefds) != 0 {
            events |= POLLOUT;
        }
        if !exceptfds.is_null() && __cluu_fd_isset(fd, exceptfds) != 0 {
            events |= POLLERR;
        }

        if events != 0 && npoll < 64 {
            poll_fds[npoll] = pollfd {
                fd,
                events,
                revents: 0,
            };
            npoll += 1;
        }
    }

    // If no fds to watch, just do a timeout sleep
    if npoll == 0 {
        if timeout_ms > 0 {
            let _ = super::time::usleep((timeout_ms as u32) * 1000);
        }
        return 0;
    }

    // Call poll()
    let result = poll(poll_fds.as_mut_ptr(), npoll, timeout_ms);
    if result < 0 {
        return -1;
    }

    // Clear output fd_sets
    if !readfds.is_null() {
        __cluu_fd_zero(readfds);
    }
    if !writefds.is_null() {
        __cluu_fd_zero(writefds);
    }
    if !exceptfds.is_null() {
        __cluu_fd_zero(exceptfds);
    }

    // Convert poll results back to fd_sets
    let mut ready = 0;
    for i in 0..npoll {
        let pfd = &poll_fds[i];
        if pfd.revents == 0 {
            continue;
        }

        let mut counted = false;

        if !readfds.is_null() && (pfd.revents & (POLLIN | POLLERR)) != 0 {
            __cluu_fd_set(pfd.fd, readfds);
            counted = true;
        }
        if !writefds.is_null() && (pfd.revents & POLLOUT) != 0 {
            __cluu_fd_set(pfd.fd, writefds);
            counted = true;
        }
        if !exceptfds.is_null() && (pfd.revents & POLLERR) != 0 {
            __cluu_fd_set(pfd.fd, exceptfds);
            counted = true;
        }

        if counted {
            ready += 1;
        }
    }

    ready
}
