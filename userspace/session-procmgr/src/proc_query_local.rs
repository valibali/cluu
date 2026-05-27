extern crate alloc;
use alloc::vec::Vec;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::SESSION_PROCMGR_PROC_QUERY_LOCAL_LABEL;
use procmgr_common::wire::{ProcInfo, ProcQueryLocalReply, ProcQueryLocalReq};
use crate::dispatch::SessionState;

pub struct ProcQueryLocal;

impl MsgHandler for ProcQueryLocal {
    const LABEL: u32 = SESSION_PROCMGR_PROC_QUERY_LOCAL_LABEL;
    type State = SessionState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let req: ProcQueryLocalReq = postcard::from_bytes(msg.payload)
            .map_err(|_| HandlerError::BadPayload)?;
        let mut procs = Vec::new();
        for c in state.child_table.iter() {
            if req.pids.is_empty() || req.pids.contains(&c.pid) {
                procs.push(ProcInfo {
                    pid: c.pid,
                    ppid: 0,
                    state: 1,
                    command: c.argv0.clone(),
                    argv0: c.argv0.clone(),
                    start_ticks: c.start_ticks,
                });
            }
        }
        let reply = ProcQueryLocalReply { procs };
        let bytes = postcard::to_allocvec(&reply).map_err(|_| HandlerError::Internal("postcard"))?;
        Ok(Reply::ok(Self::LABEL).with_payload(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::child_table::ChildState;

    #[test]
    fn empty_session_returns_empty() {
        let mut s = SessionState::new_for_test(5);
        let payload = postcard::to_allocvec(&ProcQueryLocalReq { pids: vec![] }).unwrap();
        let msg = InboundMsg {
            label: ProcQueryLocal::LABEL,
            words: [0; 6],
            payload: &payload,
            sender_tid: 1,
        };
        let r = ProcQueryLocal::handle(&mut s, &msg).unwrap();
        let reply: ProcQueryLocalReply = postcard::from_bytes(&r.payload).unwrap();
        assert!(reply.procs.is_empty());
    }

    #[test]
    fn returns_all_children() {
        let mut s = SessionState::new_for_test(5);
        for i in 0..3u64 {
            let pid = s.child_table.alloc_pid().unwrap();
            s.child_table.insert(ChildState {
                pid,
                local: (i as u32) + 1,
                thread_tok: 0,
                space_tok: 0,
                cookie: i,
                argv0: alloc::format!("p{}", i),
                start_ticks: 0,
                minted_caps: vec![],
                pgid: None,
            });
        }
        let payload = postcard::to_allocvec(&ProcQueryLocalReq { pids: vec![] }).unwrap();
        let msg = InboundMsg {
            label: ProcQueryLocal::LABEL,
            words: [0; 6],
            payload: &payload,
            sender_tid: 1,
        };
        let r = ProcQueryLocal::handle(&mut s, &msg).unwrap();
        let reply: ProcQueryLocalReply = postcard::from_bytes(&r.payload).unwrap();
        assert_eq!(reply.procs.len(), 3);
    }

    #[test]
    fn filter_specific_pids() {
        let mut s = SessionState::new_for_test(5);
        let p1 = s.child_table.alloc_pid().unwrap();
        let p2 = s.child_table.alloc_pid().unwrap();
        for (pid, name) in [(p1, "a"), (p2, "b")] {
            s.child_table.insert(ChildState {
                pid,
                local: (pid as u32) & 0x7F_FFFF,
                thread_tok: 0,
                space_tok: 0,
                cookie: pid as u64,
                argv0: name.into(),
                start_ticks: 0,
                minted_caps: vec![],
                pgid: None,
            });
        }
        let payload = postcard::to_allocvec(&ProcQueryLocalReq { pids: vec![p1] }).unwrap();
        let msg = InboundMsg {
            label: ProcQueryLocal::LABEL,
            words: [0; 6],
            payload: &payload,
            sender_tid: 1,
        };
        let r = ProcQueryLocal::handle(&mut s, &msg).unwrap();
        let reply: ProcQueryLocalReply = postcard::from_bytes(&r.payload).unwrap();
        assert_eq!(reply.procs.len(), 1);
        assert_eq!(reply.procs[0].argv0, "a");
    }
}
