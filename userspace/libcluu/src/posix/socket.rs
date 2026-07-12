//! BSD socket API wrapping IPC calls to netd.
//!
//! Follows the pipe.rs precedent: netd owns the socket table and mints
//! per-socket fds.  The client-side fd table maps POSIX fds to
//! (netd_endpoint, netd_fd) pairs.  Data flows via IPC message labels.
//!
//! The netd IPC endpoint is delivered at spawn time via TOKEN_EXTRA_0
//! (see `root-procmgr/src/main.rs` `handle_spawn_unified`).  Containers
//! whose CapProfile lacks NET never receive this token, so `socket()`
//! returns -1 with ENOSYS — the token is structurally absent, no
//! runtime ACL check (AGENTS.md §3).

use super::c_int;
use crate::boot::{process_info, TOKEN_EXTRA_0};
use crate::errno::{set_errno, EAGAIN, EBADF, EINVAL, ENOMEM, ENOSYS, ENOTSOCK};
use crate::fd_table::{FdEntry, FD_TABLE};
use crate::ipc::{
    call, call_with_reply_buf, NET_ACCEPT, NET_BIND, NET_CLOSE, NET_CONNECT, NET_DNS_RESOLVE,
    NET_LISTEN, NET_POLL, NET_RECV, NET_SEND, NET_SOCKET, NET_SOCK_TCP, NET_SOCK_UDP,
};
use crate::types::Message;
use crate::IpcFlags;

pub const AF_INET: c_int = 2;
pub const SOCK_STREAM: c_int = 1;
pub const SOCK_DGRAM: c_int = 2;

const NETD_TIMEOUT_RETRIES: usize = 200;

fn netd_endpoint() -> usize {
    process_info().tokens[TOKEN_EXTRA_0]
}

fn reply_errno(status: isize) -> c_int {
    if status < 0 {
        set_errno(status as i32);
        -1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn socket(domain: c_int, sock_type: c_int, _protocol: c_int) -> c_int {
    if domain != AF_INET {
        set_errno(EINVAL);
        return -1;
    }

    let ep = netd_endpoint();
    if ep == 0 {
        set_errno(ENOSYS);
        return -1;
    }

    let net_type = match sock_type {
        SOCK_STREAM => NET_SOCK_TCP,
        SOCK_DGRAM => NET_SOCK_UDP,
        _ => {
            set_errno(EINVAL);
            return -1;
        }
    };

    let mut msg = Message::new(NET_SOCKET, [net_type, 0, 0, 0, 0, 0], 1);
    if call(ep, &mut msg, IpcFlags::empty()).is_err() {
        set_errno(ENOMEM);
        return -1;
    }

    let netd_fd = msg.words[0];
    if netd_fd == 0 || (netd_fd as isize) < 0 {
        set_errno(if (netd_fd as isize) < 0 {
            (netd_fd as isize) as i32
        } else {
            ENOMEM
        });
        return -1;
    }

    let mut table = FD_TABLE.lock();
    let entry = FdEntry::socket(ep, netd_fd, true, true);
    table.insert(entry)
}

#[no_mangle]
pub extern "C" fn bind(fd: c_int, addr: u32, port: u16) -> c_int {
    let (ep, netd_fd) = match get_socket_fd(fd) {
        Some(v) => v,
        None => return -1,
    };

    let mut msg = Message::new(NET_BIND, [netd_fd, addr as usize, port as usize, 0, 0, 0], 1);
    if call(ep, &mut msg, IpcFlags::empty()).is_err() {
        set_errno(ENOMEM);
        return -1;
    }
    reply_errno(msg.words[0] as isize)
}

#[no_mangle]
pub extern "C" fn connect(fd: c_int, addr: u32, port: u16) -> c_int {
    let (ep, netd_fd) = match get_socket_fd(fd) {
        Some(v) => v,
        None => return -1,
    };

    let mut msg = Message::new(NET_CONNECT, [netd_fd, addr as usize, port as usize, 0, 0, 0], 1);
    for _ in 0..NETD_TIMEOUT_RETRIES {
        match call(ep, &mut msg, IpcFlags::empty()) {
            Ok(()) => break,
            Err(crate::Error::WouldBlock) => {
                let _ = crate::syscall::yield_cpu();
            }
            Err(_) => {
                set_errno(ENOMEM);
                return -1;
            }
        }
    }
    reply_errno(msg.words[0] as isize)
}

#[no_mangle]
pub extern "C" fn listen(fd: c_int, _backlog: c_int) -> c_int {
    let (ep, netd_fd) = match get_socket_fd(fd) {
        Some(v) => v,
        None => return -1,
    };

    let mut msg = Message::new(NET_LISTEN, [netd_fd, 0, 0, 0, 0, 0], 1);
    if call(ep, &mut msg, IpcFlags::empty()).is_err() {
        set_errno(ENOMEM);
        return -1;
    }
    reply_errno(msg.words[0] as isize)
}

#[no_mangle]
pub extern "C" fn accept(fd: c_int) -> c_int {
    let (ep, netd_fd) = match get_socket_fd(fd) {
        Some(v) => v,
        None => return -1,
    };

    let mut msg = Message::new(NET_ACCEPT, [netd_fd, 0, 0, 0, 0, 0], 1);
    for _ in 0..NETD_TIMEOUT_RETRIES {
        match call(ep, &mut msg, IpcFlags::empty()) {
            Ok(()) => break,
            Err(crate::Error::WouldBlock) => {
                let _ = crate::syscall::yield_cpu();
            }
            Err(_) => {
                set_errno(ENOMEM);
                return -1;
            }
        }
    }

    let status = msg.words[0] as isize;
    if status < 0 {
        set_errno(status as i32);
        return -1;
    }

    let new_netd_fd = msg.words[0];
    if new_netd_fd == 0 {
        set_errno(EAGAIN as i32);
        return -1;
    }

    let mut table = FD_TABLE.lock();
    let entry = FdEntry::socket(ep, new_netd_fd, true, true);
    table.insert(entry)
}

#[no_mangle]
pub extern "C" fn send(fd: c_int, buf: *const u8, len: usize, _flags: c_int) -> isize {
    let (ep, netd_fd) = match get_socket_fd(fd) {
        Some(v) => v,
        None => return -1,
    };

    if buf.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let data = unsafe { core::slice::from_raw_parts(buf, len) };
    let req = Message::new(NET_SEND, [len, netd_fd, 0, 0, 0, 0], 1);
    let mut reply_buf = [0u8; 4096];
    let (reply_msg, _payload_len) =
        match call_with_reply_buf(ep, &req, data, &mut reply_buf) {
            Ok(r) => r,
            Err(_) => {
                set_errno(ENOMEM);
                return -1;
            }
        };

    let sent = reply_msg.words[0] as isize;
    if sent < 0 {
        set_errno(sent as i32);
        -1
    } else {
        sent
    }
}

#[no_mangle]
pub extern "C" fn recv(fd: c_int, buf: *mut u8, len: usize, _flags: c_int) -> isize {
    let (ep, netd_fd) = match get_socket_fd(fd) {
        Some(v) => v,
        None => return -1,
    };

    if buf.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let req = Message::new(NET_RECV, [netd_fd, len, 0, 0, 0, 0], 1);
    let header_len = core::mem::size_of::<Message>();
    let mut reply_buf = {
        let size = core::mem::size_of::<Message>() + len;
        alloc::vec![0u8; size]
    };
    let (reply_msg, payload_len) =
        match call_with_reply_buf(ep, &req, &[], &mut reply_buf) {
            Ok(r) => r,
            Err(_) => {
                set_errno(ENOMEM);
                return -1;
            }
        };

    let received = reply_msg.words[0] as isize;
    if received < 0 {
        set_errno(received as i32);
        return -1;
    }

    let to_copy = (received as usize).min(len).min(payload_len);
    if to_copy > 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(
                reply_buf.as_ptr().add(header_len),
                buf,
                to_copy,
            );
        }
    }
    to_copy as isize
}

#[no_mangle]
pub extern "C" fn sendto(
    fd: c_int,
    buf: *const u8,
    len: usize,
    flags: c_int,
    addr: u32,
    port: u16,
) -> isize {
    let (ep, netd_fd) = match get_socket_fd(fd) {
        Some(v) => v,
        None => return -1,
    };

    if buf.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let data = unsafe { core::slice::from_raw_parts(buf, len) };
    let req = Message::new(
        NET_SEND,
        [len, netd_fd, addr as usize, port as usize, 0, 0],
        1,
    );
    let mut reply_buf = [0u8; 4096];
    let (reply_msg, _payload_len) =
        match call_with_reply_buf(ep, &req, data, &mut reply_buf) {
            Ok(r) => r,
            Err(_) => {
                set_errno(ENOMEM);
                return -1;
            }
        };

    let sent = reply_msg.words[0] as isize;
    if sent < 0 {
        set_errno(sent as i32);
        -1
    } else {
        let _ = flags;
        sent
    }
}

#[no_mangle]
pub extern "C" fn recvfrom(
    fd: c_int,
    buf: *mut u8,
    len: usize,
    flags: c_int,
    out_addr: *mut u32,
    out_port: *mut u16,
) -> isize {
    let received = recv(fd, buf, len, flags);
    if received >= 0 && !out_addr.is_null() && !out_port.is_null() {
        let (ep, netd_fd) = match get_socket_fd(fd) {
            Some(v) => v,
            None => return received,
        };
        let mut msg = Message::new(NET_POLL, [netd_fd, 0, 0, 0, 0, 0], 1);
        if call(ep, &mut msg, IpcFlags::empty()).is_ok() {
            unsafe {
                *out_addr = msg.words[1] as u32;
                *out_port = msg.words[2] as u16;
            }
        }
    }
    received
}

#[no_mangle]
pub extern "C" fn close_socket(fd: c_int) -> c_int {
    let (ep, netd_fd) = {
        let table = FD_TABLE.lock();
        let entry = match table.get(fd) {
            Some(e) => e,
            None => {
                set_errno(EBADF);
                return -1;
            }
        };
        if !entry.is_socket() {
            set_errno(ENOTSOCK);
            return -1;
        }
        (entry.endpoint, entry.socket_fd.unwrap_or(0))
    };

    let mut msg = Message::new(NET_CLOSE, [netd_fd, 0, 0, 0, 0, 0], 1);
    let _ = call(ep, &mut msg, IpcFlags::empty());

    let mut table = FD_TABLE.lock();
    table.remove(fd);
    0
}

/// Query socket readiness for poll().  Returns (readable, writable).
pub fn query_socket_readiness(fd: c_int) -> (bool, bool) {
    let (ep, netd_fd) = match get_socket_fd(fd) {
        Some(v) => v,
        None => return (false, false),
    };

    let mut msg = Message::new(NET_POLL, [netd_fd, 0, 0, 0, 0, 0], 1);
    if call(ep, &mut msg, IpcFlags::empty()).is_err() {
        return (false, false);
    }
    let flags = msg.words[0];
    ((flags & 1) != 0, (flags & 2) != 0)
}

fn get_socket_fd(fd: c_int) -> Option<(usize, usize)> {
    let table = FD_TABLE.lock();
    let entry = table.get(fd)?;
    if !entry.is_socket() {
        set_errno(ENOTSOCK);
        return None;
    }
    Some((entry.endpoint, entry.socket_fd.unwrap_or(0)))
}

/// Check whether the current process has a netd endpoint (NET profile).
pub fn has_netd() -> bool {
    netd_endpoint() != 0
}
pub fn net_dns_resolve(hostname: &str) -> Option<u32> {
    let ep = netd_endpoint();
    if ep == 0 {
        return None;
    }
    let msg = Message::new(NET_DNS_RESOLVE, [hostname.len(), 0, 0, 0, 0, 0], 1);
    let mut reply_buf = [0u8; 4096];
    let (reply_msg, _payload_len) =
        match call_with_reply_buf(ep, &msg, hostname.as_bytes(), &mut reply_buf) {
            Ok(r) => r,
            Err(_) => return None,
        };

    let ip_word = reply_msg.words[0];
    if (ip_word as isize) < 0 || ip_word == 0 {
        return None;
    }
    Some(ip_word as u32)
}
