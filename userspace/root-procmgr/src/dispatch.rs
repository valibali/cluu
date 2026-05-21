//! Static label → handler table.

use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use crate::session_directory::SessionDirectory;

/// Root-procmgr runtime state passed to every handler.
///
/// TODO(Phase 12): replace `MockKernel` field with `RealKernel` behind
/// `#[cfg(not(feature = "host-test"))]` once Phase 5 wires the recv loop.
pub struct ProcmgrState {
    pub session_directory: SessionDirectory,
    /// Production binary will swap this for a RealKernel in Phase 12.
    pub kernel: procmgr_common::test_kernel::MockKernel,
}

impl Default for ProcmgrState {
    fn default() -> Self { Self::new() }
}

impl ProcmgrState {
    pub fn new() -> Self {
        Self {
            session_directory: SessionDirectory::new(),
            kernel: procmgr_common::test_kernel::MockKernel::new(),
        }
    }

    #[cfg(any(test, feature = "host-test"))]
    pub fn new_for_test() -> Self { Self::new() }
}

#[allow(dead_code)]
pub fn dispatch(state: &mut ProcmgrState, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
    use crate::session_directory::{SessionCreate, SessionDestroy};
    match msg.label {
        SessionCreate::LABEL  => SessionCreate::handle(state, msg),
        SessionDestroy::LABEL => SessionDestroy::handle(state, msg),
        _ => Err(HandlerError::BadLabel),
    }
}
