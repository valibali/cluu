extern crate alloc;
use alloc::string::String;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::SESSION_PROCMGR_CTTY_QUERY_LABEL;
use crate::dispatch::SessionState;

pub struct CttyQuery;

impl MsgHandler for CttyQuery {
    const LABEL: u32 = SESSION_PROCMGR_CTTY_QUERY_LABEL;
    type State = SessionState;

    fn handle(state: &mut Self::State, _msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let ctty_path = state.ctty.clone().ok_or(HandlerError::NotFound)?;
        let bytes = postcard::to_allocvec(&ctty_path)
            .map_err(|_| HandlerError::Internal("postcard"))?;
        Ok(Reply::ok(Self::LABEL).with_payload(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::handler::InboundMsg;
    use crate::dispatch::SessionState;

    fn make_msg() -> InboundMsg<'static> {
        InboundMsg {
            label: CttyQuery::LABEL,
            words: [0; 6],
            payload: &[],
            sender_tid: 1,
        }
    }

    #[test]
    fn no_ctty_returns_notfound() {
        let mut s = SessionState::new_for_test(5);
        let msg = make_msg();
        assert!(matches!(CttyQuery::handle(&mut s, &msg), Err(HandlerError::NotFound)));
    }

    #[test]
    fn ctty_set_returns_path() {
        let mut s = SessionState::new_for_test(5);
        s.ctty = Some("/dev/pts/5".into());
        let msg = make_msg();
        let r = CttyQuery::handle(&mut s, &msg).unwrap();
        let path: String = postcard::from_bytes(&r.payload).unwrap();
        assert_eq!(path, "/dev/pts/5");
    }
}
