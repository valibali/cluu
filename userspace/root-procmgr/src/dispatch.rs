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

    // ── Primordial cap handles ────────────────────────────────────────────
    // Populated by bootstrap (Phase 12). Zero + zero-rights means the
    // sub_mint call will be a no-op safe fallback until then.
    pub parent_vfs_cap: u64,
    pub parent_vfs_rights: u32,
    pub parent_registry_cap: u64,
    pub parent_registry_rights: u32,
    pub parent_timeserver_cap: u64,
    pub parent_timeserver_rights: u32,
}

impl Default for ProcmgrState {
    fn default() -> Self { Self::new() }
}

impl ProcmgrState {
    pub fn new() -> Self {
        Self {
            session_directory: SessionDirectory::new(),
            kernel: procmgr_common::test_kernel::MockKernel::new(),
            parent_vfs_cap: 0,
            parent_vfs_rights: 0,
            parent_registry_cap: 0,
            parent_registry_rights: 0,
            parent_timeserver_cap: 0,
            parent_timeserver_rights: 0,
        }
    }

    /// Test constructor: pre-populate parent caps with non-zero handles and
    /// rights broad enough for session sub-minting.
    #[cfg(any(test, feature = "host-test"))]
    pub fn new_for_test() -> Self {
        Self {
            session_directory: SessionDirectory::new(),
            kernel: procmgr_common::test_kernel::MockKernel::new(),
            parent_vfs_cap: 0xBEEF_0001,
            parent_vfs_rights: 0x07,
            parent_registry_cap: 0xBEEF_0002,
            parent_registry_rights: 0x03,
            parent_timeserver_cap: 0xBEEF_0003,
            parent_timeserver_rights: 0x01,
        }
    }
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
