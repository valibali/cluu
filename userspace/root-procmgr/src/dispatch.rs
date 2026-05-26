//! Static label → handler table.

extern crate alloc;

use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use crate::session_directory::SessionDirectory;

fn empty_query_session_local(_sid: u8) -> procmgr_common::wire::ProcQueryLocalReply {
    procmgr_common::wire::ProcQueryLocalReply { procs: alloc::vec::Vec::new() }
}

/// Stub for `spawn_session_procmgr` that does nothing (tests + pre-wiring).
///
/// Returns `None` to signal that no real session-procmgr was spawned.
/// The `SessionCreate::handle` caller stores `(0, 0)` for `pmgr_tid` /
/// `pmgr_ep` when this stub is active, preserving the pre-Task-12.3 behaviour.
fn stub_spawn_session_procmgr(_envelope_bytes: &[u8]) -> Option<(u64, u64)> {
    None
}

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

    // ── Service registry ─────────────────────────────────────────────────
    pub services: alloc::vec::Vec<crate::services::ServiceEntry>,

    // ── Shutdown flag ─────────────────────────────────────────────────────
    pub shutting_down: bool,

    // ── Cross-session proc query ──────────────────────────────────────────
    /// Injected fn that queries a session-procmgr for its local proc list.
    /// Default stub returns empty; production wires the real IPC call.
    pub query_session_local: fn(u8) -> procmgr_common::wire::ProcQueryLocalReply,

    // ── Session-procmgr spawn callback ────────────────────────────────────
    /// Injected fn that spawns a `/bin/session-procmgr` instance and passes
    /// it the serialised `SessionEnvelope`.  Returns `Some((thread_tok, ep))`
    /// on success, `None` when the spawn is deferred (stub / pre-wiring).
    /// Production installs a real implementation; tests keep the stub.
    pub spawn_session_procmgr: fn(envelope_bytes: &[u8]) -> Option<(u64, u64)>,
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
            services: alloc::vec::Vec::new(),
            shutting_down: false,
            query_session_local: empty_query_session_local,
            spawn_session_procmgr: stub_spawn_session_procmgr,
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
            services: alloc::vec::Vec::new(),
            shutting_down: false,
            query_session_local: empty_query_session_local,
            spawn_session_procmgr: stub_spawn_session_procmgr,
        }
    }
}

#[allow(dead_code)]
pub fn dispatch(state: &mut ProcmgrState, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
    use crate::session_directory::{SessionCreate, SessionDestroy};
    use crate::services::ServiceSpawn;
    use crate::escalate::Escalate;
    use crate::shutdown::Shutdown;
    use crate::proc_query_all::ProcQueryAll;
    match msg.label {
        SessionCreate::LABEL  => SessionCreate::handle(state, msg),
        SessionDestroy::LABEL => SessionDestroy::handle(state, msg),
        ServiceSpawn::LABEL   => ServiceSpawn::handle(state, msg),
        Escalate::LABEL       => Escalate::handle(state, msg),
        Shutdown::LABEL       => Shutdown::handle(state, msg),
        ProcQueryAll::LABEL   => ProcQueryAll::handle(state, msg),
        _ => Err(HandlerError::BadLabel),
    }
}
