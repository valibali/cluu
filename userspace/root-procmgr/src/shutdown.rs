extern crate alloc;
use alloc::vec::Vec;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_SHUTDOWN_LABEL;
use procmgr_common::kernel_iface::Kernel;
use crate::dispatch::ProcmgrState;

pub const SHUTDOWN_CAP_ID: u64 = 0xCAFE_DEAD_BEEF_0001u64;

pub struct Shutdown;

impl MsgHandler for Shutdown {
    const LABEL: u32 = PROCMGR_SHUTDOWN_LABEL;
    type State = ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        if (msg.words[0] as u64) != SHUTDOWN_CAP_ID { return Err(HandlerError::BadCap); }
        // Sessions in reverse creation order
        let sids: Vec<u8> = state.session_directory.iter().map(|e| e.sid).collect();
        for sid in sids.into_iter().rev() {
            let _ = state.session_directory.mark_dying(sid);
            if let Ok(caps) = state.session_directory.finalise_dead(sid) {
                for c in caps { state.kernel.revoke(c); }
            }
        }
        // Services
        for svc in state.services.drain(..) {
            state.kernel.revoke(svc.publish_cap);
            state.kernel.revoke(svc.thread_tok);
        }
        state.shutting_down = true;
        Ok(Reply::ok(Self::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::test_kernel::KernelCall;

    #[test]
    fn missing_cap() {
        let mut s = ProcmgrState::new_for_test();
        let msg = InboundMsg { label: Shutdown::LABEL, words: [0; 6], payload: &[], sender_tid: 1 };
        assert!(matches!(Shutdown::handle(&mut s, &msg), Err(HandlerError::BadCap)));
    }

    #[test]
    fn shutdown_revokes_all() {
        let mut s = ProcmgrState::new_for_test();
        let (sid, generation) = s.session_directory.alloc_sid().unwrap();
        s.session_directory.insert(crate::session_directory::SessionEntry {
            sid, generation, user_name: "u".into(),
            session_pmgr_thread_tok: 0, session_pmgr_spawn_ep: 0,
            minted_caps: alloc::vec![0xA1, 0xA2],
            state: crate::session_directory::SessionState::Live,
        });
        s.services.push(crate::services::ServiceEntry {
            name: "vfs".into(), thread_tok: 0xCAFE1, publish_cap: 0xCAFE2,
            restart_policy: crate::restart_root::Policy::Always,
        });
        let msg = InboundMsg {
            label: Shutdown::LABEL,
            words: [SHUTDOWN_CAP_ID as usize, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        Shutdown::handle(&mut s, &msg).unwrap();
        assert!(s.shutting_down);
        let revokes = s.kernel.calls.iter().filter(|c| matches!(c, KernelCall::Revoke { .. })).count();
        assert_eq!(revokes, 4, "2 session caps + svc thread + svc publish");
    }
}
