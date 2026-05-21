extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::kernel_iface::Kernel;
use procmgr_common::labels::PROCMGR_EXIT_LABEL;
use crate::dispatch::SessionState;

pub struct ChildExit;

impl MsgHandler for ChildExit {
    const LABEL: u32 = PROCMGR_EXIT_LABEL;
    type State = SessionState;

    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let cookie = msg.words[0] as u64;
        let _exit_code = msg.words[1] as i32;
        let pid = match state.child_table.lookup_by_cookie(cookie) {
            Some(c) => c.pid,
            None => return Ok(Reply::ok(Self::LABEL)), // drop unknown silently
        };
        let child = state
            .child_table
            .remove(pid)
            .map_err(|_| HandlerError::Internal("remove"))?;
        for h in child.minted_caps {
            state.kernel.revoke(h);
        }
        Ok(Reply::ok(Self::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::handler::InboundMsg;
    use procmgr_common::test_kernel::KernelCall;
    use crate::child_table::ChildState;

    #[test]
    fn known_cookie_removes_and_revokes() {
        let mut s = SessionState::new_for_test(5);
        let pid = s.child_table.alloc_pid().unwrap();
        s.child_table.insert(ChildState {
            pid,
            local: 1,
            thread_tok: 0x100,
            cookie: 0xC0DE,
            argv0: "ls".into(),
            start_ticks: 0,
            minted_caps: alloc::vec![0xA, 0xB],
            pgid: None,
        });
        let msg = InboundMsg {
            label: ChildExit::LABEL,
            words: [0xC0DE, 0, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        ChildExit::handle(&mut s, &msg).unwrap();
        assert!(s.child_table.lookup_by_pid(pid).is_none());
        let revokes: alloc::vec::Vec<u64> = s
            .kernel
            .calls
            .iter()
            .filter_map(|c| match c {
                KernelCall::Revoke { handle } => Some(*handle),
                _ => None,
            })
            .collect();
        assert_eq!(revokes, alloc::vec![0xA, 0xB]);
    }

    #[test]
    fn unknown_cookie_drops_silently() {
        let mut s = SessionState::new_for_test(5);
        let msg = InboundMsg {
            label: ChildExit::LABEL,
            words: [0xDEAD, 0, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        ChildExit::handle(&mut s, &msg).unwrap();
        assert_eq!(s.kernel.calls.len(), 0);
    }
}
