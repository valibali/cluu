//! SYSTEM-cap-gated aggregator: collect ProcInfo from all live sessions.
//!
//! The caller must present `SYSTEM_PROC_QUERY_CAP_ID` in `msg.words[0]`.
//! Root-procmgr fans out to each session-procmgr via the injected
//! `ProcmgrState::query_session_local` fn-pointer and concatenates the results.

extern crate alloc;
use alloc::vec::Vec;

use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_PROC_QUERY_ALL_LABEL;
use procmgr_common::wire::{ProcInfo, ProcQueryLocalReply};
use crate::dispatch::ProcmgrState;

/// Capability token that gates `PROCMGR_PROC_QUERY_ALL_LABEL`.
/// Only a caller that knows this value (via escalation or SYSTEM-session minting)
/// may invoke the cross-session aggregator.
pub const SYSTEM_PROC_QUERY_CAP_ID: u64 = 0xCAFE_0000_0000_0001u64;

/// Serialised reply payload for `ProcQueryAll`.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProcQueryAllReply {
    /// Pairs of (session_id, ProcInfo) across all live sessions.
    pub procs: Vec<(u8, ProcInfo)>,
}

/// Handler for `PROCMGR_PROC_QUERY_ALL_LABEL`.
pub struct ProcQueryAll;

impl MsgHandler for ProcQueryAll {
    const LABEL: u32 = PROCMGR_PROC_QUERY_ALL_LABEL;
    type State = ProcmgrState;

    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        // ── cap check ────────────────────────────────────────────────────────
        let presented = msg.words[0] as u64;
        if presented != SYSTEM_PROC_QUERY_CAP_ID {
            return Err(HandlerError::BadCap);
        }

        // ── snapshot live session ids ─────────────────────────────────────
        // Collect into Vec first so we don't hold an iterator borrow while
        // calling query_session_local (which needs &mut state via fn-pointer).
        let snapshot: Vec<u8> = state.session_directory.iter().map(|e| e.sid).collect();

        // ── fan-out ──────────────────────────────────────────────────────
        let mut all: Vec<(u8, ProcInfo)> = Vec::new();
        for sid in snapshot {
            let reply: ProcQueryLocalReply = (state.query_session_local)(sid);
            for p in reply.procs {
                all.push((sid, p));
            }
        }

        // ── serialise ────────────────────────────────────────────────────
        let bytes = postcard::to_allocvec(&ProcQueryAllReply { procs: all })
            .map_err(|_| HandlerError::Internal("postcard"))?;
        Ok(Reply::ok(Self::LABEL).with_payload(bytes))
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_directory::{SessionEntry, SessionState};

    fn empty_local(_sid: u8) -> ProcQueryLocalReply {
        ProcQueryLocalReply { procs: alloc::vec![] }
    }

    fn one_per_session(sid: u8) -> ProcQueryLocalReply {
        ProcQueryLocalReply {
            procs: alloc::vec![ProcInfo {
                pid: ((sid as i32) << 23) | 1,
                ppid: 0,
                state: 1,
                command: "x".into(),
                argv0: "x".into(),
                start_ticks: 0,
                cpu_ticks: 0,
                heap_pages: 0,
                other_pages: 0,
            }],
        }
    }

    #[test]
    fn missing_cap_returns_badcap() {
        let mut s = ProcmgrState::new_for_test();
        s.query_session_local = empty_local;
        let msg = InboundMsg {
            label: ProcQueryAll::LABEL,
            words: [0; 6],
            payload: &[],
            sender_tid: 1,
        };
        assert!(matches!(ProcQueryAll::handle(&mut s, &msg), Err(HandlerError::BadCap)));
    }

    #[test]
    fn cap_present_returns_aggregate() {
        let mut s = ProcmgrState::new_for_test();
        // Insert two live sessions.
        for _ in 0..2 {
            let (sid, generation) = s.session_directory.alloc_sid().unwrap();
            s.session_directory.insert(SessionEntry {
                sid,
                generation,
                user_name: "u".into(),
                session_pmgr_thread_tok: 0,
                session_pmgr_spawn_ep: 0,
                minted_caps: alloc::vec![],
                state: SessionState::Live,
            });
        }
        s.query_session_local = one_per_session;
        let msg = InboundMsg {
            label: ProcQueryAll::LABEL,
            words: [SYSTEM_PROC_QUERY_CAP_ID as usize, 0, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        let r = ProcQueryAll::handle(&mut s, &msg).unwrap();
        let reply: ProcQueryAllReply = postcard::from_bytes(&r.payload).unwrap();
        assert_eq!(reply.procs.len(), 2);
    }
}
