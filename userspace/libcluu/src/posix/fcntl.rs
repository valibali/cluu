//! Minimal fcntl compatibility for common libc paths.

use super::c_int;
use crate::errno::{set_errno, EBADF, EINVAL};
use crate::fd_table::FD_TABLE;
use alloc::collections::BTreeMap;
use spin::Mutex;

// Linux-compatible command numbers used by common software.
pub const F_DUPFD: c_int = 0;
pub const F_GETFD: c_int = 1;
pub const F_SETFD: c_int = 2;
pub const F_GETFL: c_int = 3;
pub const F_SETFL: c_int = 4;

pub const FD_CLOEXEC: c_int = 1;

// Common status flags. CLUU currently only stores them; no nonblocking behavior
// is enforced by kernel paths yet.
pub const O_NONBLOCK: c_int = 0o40000; // newlib _FNONBLOCK = 0x4000
pub const O_APPEND: c_int = 0o10; // newlib _FAPPEND = 0x0008

#[derive(Default)]
struct FcntlState {
    fd_flags: BTreeMap<c_int, c_int>,
    status_flags: BTreeMap<c_int, c_int>,
}

static FCNTL_STATE: Mutex<FcntlState> = Mutex::new(FcntlState {
    fd_flags: BTreeMap::new(),
    status_flags: BTreeMap::new(),
});

pub fn fcntl_impl(fd: c_int, cmd: c_int, arg: c_int) -> c_int {
    let table = FD_TABLE.lock();
    if !table.contains(fd) {
        set_errno(EBADF);
        return -1;
    }
    drop(table);

    let mut state = FCNTL_STATE.lock();
    match cmd {
        F_DUPFD => {
            let min_fd = arg.max(0);
            let mut table = FD_TABLE.lock();
            let entry = match table.get(fd) {
                Some(e) => e.clone(),
                None => {
                    set_errno(EBADF);
                    return -1;
                }
            };
            let mut new_fd = min_fd;
            while table.contains(new_fd) {
                new_fd += 1;
            }
            table.insert_at(new_fd, entry);
            if let Some(flags) = state.fd_flags.get(&fd).copied() {
                state.fd_flags.insert(new_fd, flags);
            }
            if let Some(flags) = state.status_flags.get(&fd).copied() {
                state.status_flags.insert(new_fd, flags);
            }
            new_fd
        }
        F_GETFD => *state.fd_flags.get(&fd).unwrap_or(&0),
        F_SETFD => {
            state.fd_flags.insert(fd, arg & FD_CLOEXEC);
            0
        }
        F_GETFL => *state.status_flags.get(&fd).unwrap_or(&0),
        F_SETFL => {
            let allowed = O_NONBLOCK | O_APPEND;
            state.status_flags.insert(fd, arg & allowed);
            0
        }
        _ => {
            set_errno(EINVAL);
            -1
        }
    }
}
