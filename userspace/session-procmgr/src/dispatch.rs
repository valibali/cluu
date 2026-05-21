extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::pid::SessionId;
use procmgr_common::test_kernel::MockKernel;

pub struct SessionState {
    pub sid: SessionId,
    pub generation: u32,
    pub child_table: crate::child_table::ChildTable,
    pub kernel: MockKernel,
    pub vfs_cap: u64,
    pub registry_cap: u64,
    pub timeserver_cap: u64,
}

impl SessionState {
    pub fn new_for_test(sid: SessionId) -> Self {
        Self {
            sid,
            generation: 0,
            child_table: crate::child_table::ChildTable::new(sid),
            kernel: MockKernel::new(),
            vfs_cap: 0xF000,
            registry_cap: 0xF001,
            timeserver_cap: 0xF002,
        }
    }
}

pub fn dispatch(state: &mut SessionState, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
    match msg.label {
        procmgr_common::labels::SESSION_PROCMGR_SPAWN_LABEL => {
            crate::spawn::Spawn::handle(state, msg)
        }
        procmgr_common::labels::PROCMGR_EXIT_LABEL => {
            crate::child_monitor::ChildExit::handle(state, msg)
        }
        _ => Err(HandlerError::BadLabel),
    }
}
