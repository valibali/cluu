//! `PROCMGR_PROC_QUERY_LABEL` handler for session-procmgr.
//!
//! Answers `/proc/<tid>/{status,stat,cmdline,comm,exe}` queries from VFS.
//! Looks up children by `child_tid` (the kernel TID from `thread_enumerate`).
//!
//! Cap-model: pure possession. If the TID is in our `child_table`, we
//! respond. If not, `NotFound`. No identity checks, no ACL.

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;

use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use libcluu::ipc::PROCMGR_PROC_QUERY_LABEL;
use libcluu::syscall::{space_get_stats, thread_get_stats};
use libcluu::Error;

use crate::dispatch::SessionState;

// Query type constants — match root-procmgr + VFS procfs.
const QUERY_STATUS: usize = 0;
const QUERY_STAT: usize = 1;
const QUERY_CMDLINE: usize = 2;
const QUERY_COMM: usize = 4;
const QUERY_EXE: usize = 5;

/// Maximum process name length for /proc/<pid>/comm (Linux TASK_COMM_LEN-1).
const COMM_MAX: usize = 15;

pub struct ProcQuery;

impl MsgHandler for ProcQuery {
    const LABEL: u32 = PROCMGR_PROC_QUERY_LABEL;
    type State = SessionState;

    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let query_type = msg.words[0];
        let raw_target = msg.words[1];
        let original_caller_tid = msg.words[2];

        // Resolve target TID: if raw_target != 0, it's a TID from
        // /proc/<tid>/... If raw_target == 0, it's "self" → use the
        // caller's TID.
        let target_tid = if raw_target != 0 {
            raw_target
        } else {
            original_caller_tid
        };

        // Find the child whose kernel TID matches.
        // Extract owned data so we release the borrow on child_table before
        // calling kernel stats syscalls.
        let child_data = state
            .child_table
            .iter()
            .find(|c| c.child_tid == target_tid)
            .map(|c| {
                (
                    c.argv0.clone(),
                    c.pid,
                    c.thread_tok,
                    c.space_tok,
                    c.parent_pid,
                )
            });

        let (name, pid, thread_tok, space_tok, parent_pid) = match child_data {
            Some(d) => d,
            None => {
                // NotFound: return a reply with errno so VFS can fall
                // through to other procmgrs. Using Ok(Reply) instead of
                // Err(HandlerError) so the wire format carries the errno
                // in the right word slot.
                return Ok(not_found());
            }
        };

        // Query type 3 (list) is handled by readdir, not here.
        if query_type == 3 {
            return Ok(not_found());
        }

        let sid = state.sid as usize;

        match query_type {
            QUERY_STATUS => {
                let content = format!(
                    "Name:\t{}\nPid:\t{}\nState:\tR\nSession:\t{}\n",
                    name, pid, sid,
                );
                Ok(success(content.into_bytes()))
            }
            QUERY_STAT => {
                let cpu_ticks = thread_get_stats(thread_tok as usize).unwrap_or(0);
                let (code_pages, heap_pages, stack_pages) =
                    space_get_stats(space_tok as usize).unwrap_or((0, 0, 0));
                let other_pages = code_pages.saturating_add(stack_pages);
                // Format matches root-procmgr:
                //   pid (name) state cpu_ticks heap_pages other_pages ppid sid cid pcid
                // cid = own PID (session-scoped), pcid = parent's PID so top
                // nests children under their parent.
                let content = format!(
                    "{} ({}) R {} {} {} 0 {} {} {}\n",
                    pid, name, cpu_ticks, heap_pages, other_pages, sid, pid, parent_pid,
                );
                Ok(success(content.into_bytes()))
            }
            QUERY_CMDLINE => {
                let mut content = Vec::with_capacity(name.len() + 1);
                content.extend_from_slice(name.as_bytes());
                content.push(0);
                Ok(success(content))
            }
            QUERY_COMM => {
                let trimmed = if name.len() > COMM_MAX {
                    &name[..COMM_MAX]
                } else {
                    &name
                };
                let content = format!("{}\n", trimmed);
                Ok(success(content.into_bytes()))
            }
            QUERY_EXE => {
                let content = format!("{}\n", name);
                Ok(success(content.into_bytes()))
            }
            _ => Ok(not_found()),
        }
    }
}

/// Build a success reply: errno=0 at words[0] (maps to raw words[1] in
/// session-procmgr's `send_reply`), payload = content bytes.
fn success(content: Vec<u8>) -> Reply {
    Reply::ok(PROCMGR_PROC_QUERY_LABEL)
        .with_word(0, 0)
        .with_payload(content)
}

/// Build a NotFound reply. errno goes in words[1] because session-procmgr's
/// `send_reply` copies `r.words[i]`→`msg.words[i]` for header-only replies,
/// and VFS reads errno from `reply.words[1]`.
fn not_found() -> Reply {
    let errno = Error::NotFound.to_errno() as usize;
    Reply::ok(PROCMGR_PROC_QUERY_LABEL)
        .with_word(0, errno)
        .with_word(1, errno)
}
