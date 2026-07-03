extern crate alloc;
use alloc::string::String;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::kernel_iface::Kernel;
use procmgr_common::pid::SessionId;
#[cfg(feature = "host-test")]
use procmgr_common::test_kernel::MockKernel;

/// The kernel type used at runtime depends on whether we are in a host-test
/// build (MockKernel) or a real target build (RealKernel).
#[cfg(feature = "host-test")]
pub type KernelImpl = MockKernel;
#[cfg(not(feature = "host-test"))]
pub type KernelImpl = crate::real_kernel::RealKernel;

pub struct SessionState {
    pub sid: SessionId,
    pub generation: u32,
    pub child_table: crate::child_table::ChildTable,
    pub kernel: KernelImpl,
    pub vfs_cap: u64,
    pub registry_cap: u64,
    pub timeserver_cap: u64,
    pub restart: crate::restart::RestartTracker,
    pub pipes: crate::pipe_registry::PipeRegistry,
    pub ctty: Option<String>,
    /// Production only: the endpoint on which this session-procmgr listens.
    /// Children receive an IPC_SEND-narrowed derivative as their exit_token.
    /// Zero in host-test builds (MockKernel handles spawn there).
    pub spawn_ep: u64,
    pub view_mgr_token: u64,
    pub pg_table: crate::pg_table::PgTable,
}

#[cfg(feature = "host-test")]
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
            restart: crate::restart::RestartTracker::new(),
            pipes: crate::pipe_registry::PipeRegistry::new(),
            ctty: None,
            spawn_ep: 0,
            view_mgr_token: 0,
            pg_table: crate::pg_table::PgTable::new(),
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
        procmgr_common::labels::SESSION_PROCMGR_PIPE_CREATE_LABEL => {
            crate::pipe_handlers::PipeCreate::handle(state, msg)
        }
        procmgr_common::labels::SESSION_PROCMGR_PIPE_CLOSE_LABEL => {
            crate::pipe_handlers::PipeClose::handle(state, msg)
        }
        procmgr_common::labels::SESSION_PROCMGR_KILL_LABEL => {
            crate::kill::Kill::handle(state, msg)
        }
        procmgr_common::labels::SESSION_PROCMGR_CTTY_QUERY_LABEL => {
            crate::ctty::CttyQuery::handle(state, msg)
        }
        procmgr_common::labels::SESSION_PROCMGR_PROC_QUERY_LOCAL_LABEL => {
            crate::proc_query_local::ProcQueryLocal::handle(state, msg)
        }
        libcluu::ipc::PROCMGR_PG_CREATE_LABEL => {
            let pgid = state.pg_table.create();
            Ok(Reply::ok(libcluu::ipc::PROCMGR_PG_CREATE_LABEL).with_word(0, pgid))
        }
        libcluu::ipc::PROCMGR_PG_ATTACH_LABEL => {
            let pgid = msg.words[0];
            let pid = msg.words[1] as i32;
            state.pg_table.attach(pgid, pid as usize);
            state.child_table.set_pgid(pid, pgid as u32);
            Ok(Reply::ok(libcluu::ipc::PROCMGR_PG_ATTACH_LABEL))
        }
        libcluu::ipc::PROCMGR_PG_SIGNAL_LABEL => {
            let pgid = msg.words[0];
            let signum = msg.words[1];
            let SIGINT: usize = 2;
            let SIGTERM: usize = 15;
            let SIGKILL: usize = 9;
            let pids = state.pg_table.members(pgid);
            for pid in pids {
                if let Some(child) = state.child_table.lookup_by_pid(pid as i32) {
                    let tok = child.thread_tok;
                    let notify_ep = child.notify_ep;
                    let cookie = child.cookie;
                    match signum {
                        s if s == SIGINT || s == SIGTERM || s == SIGKILL => {
                            state.kernel.thread_destroy(tok);
                            if notify_ep != 0 {
                                let exit_code = 128 + signum as i32;
                                let exit_msg = libcluu::types::Message::new(
                                    procmgr_common::labels::PROCMGR_EXIT_LABEL,
                                    [cookie as usize, exit_code as usize, 0, 0, 0, 0],
                                    2,
                                );
                                let _ = libcluu::ipc::send(notify_ep as usize, &exit_msg, libcluu::IpcFlags::empty());
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Reply::ok(libcluu::ipc::PROCMGR_PG_SIGNAL_LABEL))
        }
        libcluu::ipc::PROCMGR_PROC_QUERY_LABEL => {
            crate::proc_query::ProcQuery::handle(state, msg)
        }
        _ => Err(HandlerError::BadLabel),
    }
}
