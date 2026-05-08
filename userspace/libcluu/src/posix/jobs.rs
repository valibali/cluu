//! Process-group IPC wrappers (Phase 4 Plan D job control).
//!
//! These are thin wrappers around the PROCMGR_PG_* IPC labels.  They expect a
//! valid procmgr endpoint handle obtained from the process's token slots.

use crate::ipc::{
    self,
    PROCMGR_PG_ATTACH_LABEL,
    PROCMGR_PG_CREATE_LABEL,
    PROCMGR_PG_RESUME_LABEL,
    PROCMGR_PG_SIGNAL_LABEL,
    PROCMGR_PG_SUSPEND_LABEL,
    PROCMGR_PID_PGID_QUERY_LABEL,
};
use crate::types::{IpcFlags, Message};
use crate::Result;

/// Allocate a new process group. Returns the new pgid on success.
pub fn pg_create(procmgr_ep: usize) -> Result<usize> {
    let mut msg = Message::new(PROCMGR_PG_CREATE_LABEL, [0; 6], 0);
    ipc::call(procmgr_ep, &mut msg, IpcFlags::empty())?;
    Ok(msg.words[1])
}

/// Attach `pid` to an existing `pgid`. Fire-and-forget.
pub fn pg_attach(procmgr_ep: usize, pgid: usize, pid: usize) -> Result<()> {
    let mut msg = Message::new(PROCMGR_PG_ATTACH_LABEL, [pgid, pid, 0, 0, 0, 0], 2);
    ipc::send(procmgr_ep, &msg, IpcFlags::empty())
}

/// Deliver signal `signum` to all members of `pgid`. Fire-and-forget.
pub fn pg_signal(procmgr_ep: usize, pgid: usize, signum: i32) -> Result<()> {
    let mut msg = Message::new(
        PROCMGR_PG_SIGNAL_LABEL,
        [pgid, signum as usize, 0, 0, 0, 0],
        2,
    );
    ipc::send(procmgr_ep, &msg, IpcFlags::empty())
}

/// Suspend all threads of every pid in `pgid`. Fire-and-forget.
pub fn pg_suspend(procmgr_ep: usize, pgid: usize) -> Result<()> {
    let mut msg = Message::new(PROCMGR_PG_SUSPEND_LABEL, [pgid, 0, 0, 0, 0, 0], 1);
    ipc::send(procmgr_ep, &msg, IpcFlags::empty())
}

/// Resume all threads of every pid in `pgid`. Fire-and-forget.
pub fn pg_resume(procmgr_ep: usize, pgid: usize) -> Result<()> {
    let mut msg = Message::new(PROCMGR_PG_RESUME_LABEL, [pgid, 0, 0, 0, 0, 0], 1);
    ipc::send(procmgr_ep, &msg, IpcFlags::empty())
}

/// Query the pgid of the process that owns thread `tid`.
/// Returns 0 if the thread is not a member of any process group.
pub fn pgid_of(procmgr_ep: usize, tid: usize) -> Result<usize> {
    let mut msg = Message::new(PROCMGR_PID_PGID_QUERY_LABEL, [tid, 0, 0, 0, 0, 0], 1);
    ipc::call(procmgr_ep, &mut msg, IpcFlags::empty())?;
    Ok(msg.words[1])
}
