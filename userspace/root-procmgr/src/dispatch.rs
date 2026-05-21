//! Static label → handler table. New handlers register here as they migrate
//! out of the legacy inline impl in `main.rs`.

use procmgr_common::handler::{HandlerError, InboundMsg, Reply};

// allow until Phase N handlers register
#[allow(dead_code)]
pub struct ProcmgrState; // grown in later phases

// allow until Phase N handlers register
#[allow(dead_code)]
pub fn dispatch(_state: &mut ProcmgrState, _msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
    Err(HandlerError::BadLabel)
}
