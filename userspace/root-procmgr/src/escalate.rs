//! Escalation: hand a holder of an "escalate-cap" a SYSTEM cap-bundle (e.g. for sudo).
//! Strict cap model — escalate-cap is what gates, no identity lookup.

extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_ESCALATE_LABEL;
use procmgr_common::kernel_iface::Kernel;
use crate::dispatch::ProcmgrState;

pub const ESCALATE_CAP_ID: u64 = 0xCAFE_E5CA_1A7E_0001u64;

pub struct Escalate;

impl MsgHandler for Escalate {
    const LABEL: u32 = PROCMGR_ESCALATE_LABEL;
    type State = ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        if (msg.words[0] as u64) != ESCALATE_CAP_ID { return Err(HandlerError::BadCap); }
        let granted = state.kernel.mint(0xBEEF_5BEEFu64, 0xFFFF_FFFF);
        Ok(Reply::ok(Self::LABEL).with_word(0, granted as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cap() {
        let mut s = ProcmgrState::new_for_test();
        let msg = InboundMsg { label: Escalate::LABEL, words: [0; 6], payload: &[], sender_tid: 1 };
        assert!(matches!(Escalate::handle(&mut s, &msg), Err(HandlerError::BadCap)));
    }

    #[test]
    fn cap_present_grants_bundle() {
        let mut s = ProcmgrState::new_for_test();
        let msg = InboundMsg {
            label: Escalate::LABEL,
            words: [ESCALATE_CAP_ID as usize, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        let r = Escalate::handle(&mut s, &msg).unwrap();
        assert_ne!(r.words[0], 0);
    }
}
