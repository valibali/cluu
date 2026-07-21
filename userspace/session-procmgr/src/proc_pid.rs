//! PID-keyed IPC handlers for /proc queries.
//!
//! Wire convention (both labels):
//!   - words[0] = errno (0 on success, non-zero on failure)
//!   - list_pids: words[1] = pid_count, payload = raw LE u32 PIDs
//!   - proc_info: payload = postcard(ProcInfo) on hit, empty on miss

extern crate alloc;

use alloc::vec::Vec;
use procmgr_common::handler::{InboundMsg, Reply};
use procmgr_common::pid::Pid;
use procmgr_common::wire::ProcInfo;

use libcluu::ipc::{PROCMGR_LIST_PIDS_LABEL, PROCMGR_PROC_INFO_LABEL};
use libcluu::syscall::{thread_get_stats, space_get_stats};
use libcluu::Result;

use crate::dispatch::SessionState;

pub fn list_pids_handler(state: &SessionState, _msg: &InboundMsg<'_>) -> Result<Reply> {
    let pids: Vec<u32> = state.child_table.iter().map(|c| c.pid as u32).collect();
    let pid_count = pids.len() as usize;

    let mut payload: Vec<u8> = Vec::with_capacity(pids.len() * 4);
    for pid in pids {
        payload.extend_from_slice(&pid.to_le_bytes());
    }

    Ok(Reply::ok(PROCMGR_LIST_PIDS_LABEL)
        .with_word(0, 0)
        .with_word(1, pid_count)
        .with_payload(payload))
}

pub fn proc_info_handler(state: &SessionState, msg: &InboundMsg<'_>) -> Result<Reply> {
    // words[0] is payload_len (clobbered by IpcCallFuture); pid is in words[1].
    let pid = msg.words[1] as Pid;

    let child = state.child_table.lookup_by_pid(pid);

    match child {
        Some(c) => {
            let cpu_ticks = thread_get_stats(c.thread_tok as usize).unwrap_or(0);
            let (code_pages, heap_pages, stack_pages) =
                space_get_stats(c.space_tok as usize).unwrap_or((0, 0, 0));
            let other_pages = code_pages.saturating_add(stack_pages);
            let info = ProcInfo {
                pid: c.pid,
                ppid: c.parent_pid,
                state: 1,
                command: c.argv0.clone(),
                argv0: c.argv0.clone(),
                start_ticks: c.start_ticks,
                cpu_ticks,
                heap_pages: heap_pages as u32,
                other_pages: other_pages as u32,
            };
            let bytes = postcard::to_allocvec(&info)
                .map_err(|_| libcluu::Error::InvalidArgument)?;
            Ok(Reply::ok(PROCMGR_PROC_INFO_LABEL)
                .with_word(0, 0)
                .with_payload(bytes))
        }
        None => {
            Ok(Reply::ok(PROCMGR_PROC_INFO_LABEL)
                .with_word(0, libcluu::Error::NotFound.to_errno() as usize))
        }
    }
}
