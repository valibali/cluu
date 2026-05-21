extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::kernel_iface::Kernel;
use procmgr_common::labels::SESSION_PROCMGR_KILL_LABEL;
use procmgr_common::pid::Pid;
use crate::dispatch::SessionState;

pub struct Kill;

impl MsgHandler for Kill {
    const LABEL: u32 = SESSION_PROCMGR_KILL_LABEL;
    type State = SessionState;

    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let pid = msg.words[0] as Pid;
        let signal = msg.words[1] as u32;
        let child = state
            .child_table
            .lookup_by_pid(pid)
            .ok_or(HandlerError::NotFound)?;
        if signal == 9 {
            let tok = child.thread_tok;
            state.kernel.revoke(tok);
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
    use crate::dispatch::SessionState;

    fn make_child(state: &mut SessionState, thread_tok: u64) -> Pid {
        let pid = state.child_table.alloc_pid().unwrap();
        let local = (pid as u32) & procmgr_common::pid::LOCAL_MAX;
        state.child_table.insert(ChildState {
            pid,
            local,
            thread_tok,
            cookie: 0xC0DE_0000 ^ pid as u64,
            argv0: "ls".into(),
            start_ticks: 0,
            minted_caps: alloc::vec![],
            pgid: None,
        });
        pid
    }

    #[test]
    fn kill_sigkill_revokes_thread() {
        let mut s = SessionState::new_for_test(5);
        let tok: u64 = 0xBEEF_0001;
        let pid = make_child(&mut s, tok);
        let msg = InboundMsg {
            label: Kill::LABEL,
            words: [pid as usize, 9, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        Kill::handle(&mut s, &msg).unwrap();
        assert!(
            s.kernel.calls.iter().any(|c| matches!(c, KernelCall::Revoke { handle } if *handle == tok)),
            "expected Revoke({tok:#x}) in kernel call log"
        );
    }

    #[test]
    fn kill_unknown_pid_returns_notfound() {
        let mut s = SessionState::new_for_test(5);
        // Use a pid that was never inserted.
        let msg = InboundMsg {
            label: Kill::LABEL,
            words: [0x2800_9999usize, 9, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        assert!(matches!(Kill::handle(&mut s, &msg), Err(HandlerError::NotFound)));
    }

    #[test]
    fn kill_pid_from_other_session_returns_notfound() {
        let mut s = SessionState::new_for_test(5);
        // Encode a pid for session 7 — not our session (5).
        let pid: Pid = procmgr_common::pid::encode(7, 1).unwrap();
        let msg = InboundMsg {
            label: Kill::LABEL,
            words: [pid as usize, 9, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        assert!(matches!(Kill::handle(&mut s, &msg), Err(HandlerError::NotFound)));
    }
}
