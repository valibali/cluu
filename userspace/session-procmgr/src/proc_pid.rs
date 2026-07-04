//! PID-keyed IPC handlers that reply directly via `ipc_send` to an embedded
//! reply endpoint (words[4]) rather than via `ipc_reply` on the receive
//! channel. Used by async callers that cannot block on a synchronous
//! call/reply round-trip.
//!
//! Wire convention (both labels):
//!   - words[4] = reply_ep   (capability token the handler sends to)
//!   - words[5] = caller_cookie (echoed back unchanged in words[5])
//!   - words[0] = errno      (0 on success, non-zero on failure)
//!
//! `list_pids`: payload = raw little-endian u32 PIDs, words[1] = pid_count.
//! `proc_info`: payload = postcard(ProcInfo) on hit, empty on miss.

extern crate alloc;

use alloc::vec::Vec;
use procmgr_common::handler::InboundMsg;
use procmgr_common::pid::Pid;
use procmgr_common::wire::ProcInfo;

use libcluu::ipc::{PROCMGR_LIST_PIDS_LABEL, PROCMGR_PROC_INFO_LABEL};
use libcluu::syscall::ipc_send;
use libcluu::types::Message;
use libcluu::Result;

use crate::dispatch::SessionState;

pub fn list_pids_handler(state: &SessionState, msg: &InboundMsg<'_>) -> Result<()> {
    let reply_ep = msg.words[4];
    let cookie = msg.words[5] as u64;

    let mut pids: Vec<u32> = state.child_table.iter().map(|c| c.pid as u32).collect();
    let pid_count = pids.len() as usize;

    let mut payload: Vec<u8> = Vec::with_capacity(pids.len() * 4);
    for pid in pids.drain(..) {
        payload.extend_from_slice(&pid.to_le_bytes());
    }

    let reply = Message::new(
        PROCMGR_LIST_PIDS_LABEL,
        [0, pid_count, 0, 0, 0, cookie as usize],
        6,
    );
    send_with_payload(reply_ep, &reply, &payload)
}

pub fn proc_info_handler(state: &SessionState, msg: &InboundMsg<'_>) -> Result<()> {
    let pid = msg.words[0] as Pid;
    let reply_ep = msg.words[4];
    let cookie = msg.words[5] as u64;

    let child = state.child_table.lookup_by_pid(pid);

    let (errno, payload): (usize, Vec<u8>) = match child {
        Some(c) => {
            let info = ProcInfo {
                pid: c.pid,
                ppid: c.parent_pid,
                state: 1,
                command: c.argv0.clone(),
                argv0: c.argv0.clone(),
                start_ticks: c.start_ticks,
            };
            let bytes = postcard::to_allocvec(&info)
                .map_err(|_| libcluu::Error::InvalidArgument)?;
            (0, bytes)
        }
        None => (libcluu::Error::NotFound.to_errno() as usize, Vec::new()),
    };

    let reply = Message::new(
        PROCMGR_PROC_INFO_LABEL,
        [errno, 0, 0, 0, 0, cookie as usize],
        6,
    );
    send_with_payload(reply_ep, &reply, &payload)
}

fn send_with_payload(reply_ep: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let header = msg.as_bytes();
    let mut buf: Vec<u8> = Vec::with_capacity(header.len() + payload.len());
    buf.extend_from_slice(header);
    buf.extend_from_slice(payload);
    ipc_send(reply_ep, &buf)
}
