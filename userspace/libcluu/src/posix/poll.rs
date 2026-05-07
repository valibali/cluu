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

/// Maximum number of TTY file descriptors tracked per poll() call.
const MAX_TTY_POLL_FDS: usize = 4;

/// Maximum number of pipe read-end fds tracked per poll() call.
const MAX_PIPE_POLL_FDS: usize = 16;

/// Query the TTY service for input data readiness.
///
/// Uses the TTY endpoint cached from fd 1 (stdout), NOT fd 0's endpoint,
/// because fd 0's endpoint is the process's own receive endpoint — calling
/// ipc_call on it would deadlock.
///
/// Returns `true` if the TTY reports data in its input queue.
/// Returns `false` on any error (TTY not started, IPC failure, etc.).
fn query_tty_readiness() -> bool {
    use crate::ipc::TTY_POLL_QUERY_LABEL;
    use crate::types::Message;

    let tty_ep = match super::file::get_tty_endpoint() {
        Some(ep) => ep,
        None => return false,
    };

    let msg = Message::new(TTY_POLL_QUERY_LABEL, [0; 6], 0);
    let mut reply_buf = [0u8; 512];
    match crate::syscall::ipc_call(tty_ep, msg.as_bytes(), &mut reply_buf) {
        Ok(bytes) if bytes >= core::mem::size_of::<Message>() => {
            let reply_msg = unsafe { (reply_buf.as_ptr() as *const Message).read_unaligned() };
            reply_msg.words[0] != 0
        }
        _ => false,
    }
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

        // Phase 1: Check non-IPC readiness under FD_TABLE lock.
        // Collect indices of TTY/pipe fds that need a readiness IPC query.
        let mut tty_pollin_indices = [0usize; MAX_TTY_POLL_FDS];
        let mut n_tty = 0usize;
        let mut pipe_pollin_indices = [0usize; MAX_PIPE_POLL_FDS];
        let mut pipe_endpoints = [0usize; MAX_PIPE_POLL_FDS];
        let mut n_pipe = 0usize;

        if nfds > 0 {
            let entries = unsafe { core::slice::from_raw_parts_mut(fds, nfds) };
            let table = FD_TABLE.lock();
            for (i, pfd) in entries.iter_mut().enumerate() {
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
                    // Regular seekable file descriptors (incl. /dev pseudo-files
                    // backed by VFS DeviceBackend) are always readable when the
                    // fd was opened readable.
                    if entry.is_seekable() && entry.is_readable() {
                        pfd.revents |= POLLIN;
                    } else if entry.is_pipe()
                        && entry.is_readable()
                        && n_pipe < MAX_PIPE_POLL_FDS
                    {
                        pipe_pollin_indices[n_pipe] = i;
                        pipe_endpoints[n_pipe] = entry.endpoint;
                        n_pipe += 1;
                    } else if entry.is_tty() && entry.is_readable() && n_tty < MAX_TTY_POLL_FDS {
                        tty_pollin_indices[n_tty] = i;
                        n_tty += 1;
                    }
                }
                if (pfd.events & POLLOUT) != 0 && entry.is_writable() {
                    // Pipes, TTYs, files: writes do not block in current
                    // implementation under common-case load (queue MAX = 1024
                    // messages). Treat POLLOUT as always-ready for any writable
                    // fd. If write actually blocks, callers will discover via
                    // EAGAIN when O_NONBLOCK lands; until then this matches the
                    // behavior we want for shell pipelines.
                    pfd.revents |= POLLOUT;
                }
                if pfd.revents != 0 {
                    ready += 1;
                }
            }
            drop(table); // Must drop FD_TABLE lock before Phase 2 IPC calls

            // Phase 2a: Query TTY readiness via IPC (outside lock).
            if n_tty > 0 && query_tty_readiness() {
                for j in 0..n_tty {
                    let pfd = &mut entries[tty_pollin_indices[j]];
                    if pfd.revents == 0 {
                        ready += 1;
                    }
                    pfd.revents |= POLLIN;
                }
            }

            // Phase 2b: Query pipe read-end readiness via EndpointPeek invoke
            // (outside lock). Each pipe read-end token has IPC_RECV right; peek
            // is non-destructive and returns the queue depth.
            for j in 0..n_pipe {
                let depth = crate::syscall::endpoint_peek(pipe_endpoints[j]).unwrap_or(0);
                if depth > 0 {
                    let pfd = &mut entries[pipe_pollin_indices[j]];
                    if pfd.revents == 0 {
                        ready += 1;
                    }
                    pfd.revents |= POLLIN;
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
    // Max pollfd array size; reject if more fds are active than we can handle
    const MAX_POLL_FDS: usize = 64;

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
        let ms = tv
            .tv_sec
            .saturating_mul(1000)
            .saturating_add(tv.tv_usec / 1000);
        if ms > i32::MAX as i64 {
            i32::MAX
        } else {
            ms as c_int
        }
    };

    // Build pollfd array from fd_sets
    let mut poll_fds: [pollfd; MAX_POLL_FDS] = [pollfd {
        fd: -1,
        events: 0,
        revents: 0,
    }; MAX_POLL_FDS];
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

        if events != 0 {
            if npoll >= MAX_POLL_FDS {
                set_errno(EINVAL);
                return -1;
            }
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
    for pfd in poll_fds.iter().take(npoll) {
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
