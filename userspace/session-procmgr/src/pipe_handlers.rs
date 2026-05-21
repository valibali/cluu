extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::kernel_iface::Kernel;
use crate::dispatch::SessionState;
use procmgr_common::labels::{
    SESSION_PROCMGR_PIPE_CREATE_LABEL,
    SESSION_PROCMGR_PIPE_CLOSE_LABEL,
};

pub struct PipeCreate;
impl MsgHandler for PipeCreate {
    const LABEL: u32 = SESSION_PROCMGR_PIPE_CREATE_LABEL;
    type State = SessionState;
    fn handle(state: &mut Self::State, _msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        // Production: allocate shared buffer, mint read/write caps with appropriate rights.
        // Test path: use mock kernel mints.
        let buffer_cap = state.kernel.mint(0xBEEF_B0F, 0xFF);
        let read_cap = state.kernel.mint(buffer_cap, 0x01);  // read-only
        let write_cap = state.kernel.mint(buffer_cap, 0x02); // write-only
        let id = state.pipes.create(read_cap, write_cap, buffer_cap);
        Ok(Reply::ok(Self::LABEL).with_word(0, id as usize))
    }
}

pub struct PipeClose;
impl MsgHandler for PipeClose {
    const LABEL: u32 = SESSION_PROCMGR_PIPE_CLOSE_LABEL;
    type State = SessionState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let id = msg.words[0] as u64;
        let pipe = state.pipes.close(id).map_err(|_| HandlerError::NotFound)?;
        state.kernel.revoke(pipe.read_cap);
        state.kernel.revoke(pipe.write_cap);
        state.kernel.revoke(pipe.buffer_cap);
        Ok(Reply::ok(Self::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::test_kernel::KernelCall;

    #[test]
    fn create_returns_id_and_mints_three_caps() {
        let mut s = SessionState::new_for_test(5);
        let msg = InboundMsg {
            label: PipeCreate::LABEL,
            words: [0; 6],
            payload: &[],
            sender_tid: 1,
        };
        let reply = PipeCreate::handle(&mut s, &msg).unwrap();
        assert_ne!(reply.words[0], 0);
        let mints = s.kernel.calls.iter()
            .filter(|c| matches!(c, KernelCall::Mint { .. }))
            .count();
        assert_eq!(mints, 3, "buffer + read + write caps");
    }

    #[test]
    fn close_revokes_three_caps() {
        let mut s = SessionState::new_for_test(5);
        let create_msg = InboundMsg {
            label: PipeCreate::LABEL,
            words: [0; 6],
            payload: &[],
            sender_tid: 1,
        };
        let r = PipeCreate::handle(&mut s, &create_msg).unwrap();
        let id = r.words[0] as u64;
        let close_msg = InboundMsg {
            label: PipeClose::LABEL,
            words: [id as usize, 0, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        PipeClose::handle(&mut s, &close_msg).unwrap();
        let revokes = s.kernel.calls.iter()
            .filter(|c| matches!(c, KernelCall::Revoke { .. }))
            .count();
        assert_eq!(revokes, 3);
    }

    #[test]
    fn close_unknown_returns_notfound() {
        let mut s = SessionState::new_for_test(5);
        let msg = InboundMsg {
            label: PipeClose::LABEL,
            words: [9999, 0, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        assert!(matches!(PipeClose::handle(&mut s, &msg), Err(HandlerError::NotFound)));
    }
}
