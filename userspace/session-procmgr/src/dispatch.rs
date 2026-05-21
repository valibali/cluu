extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, Reply};
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

pub fn dispatch(_state: &mut SessionState, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
    match msg.label {
        _ => Err(HandlerError::BadLabel),
    }
}
