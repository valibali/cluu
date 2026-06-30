extern crate alloc;
use alloc::vec::Vec;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::kernel_iface::Kernel;
use procmgr_common::labels::SESSION_PROCMGR_SPAWN_LABEL;
use procmgr_common::mint_guard::MintGuard;
use procmgr_common::pid::LOCAL_MAX;
use procmgr_common::wire::{SpawnReply, SpawnReq};
use crate::cap_broker_session::{sub_mint, CapRights};
use crate::child_table::ChildState;
use crate::dispatch::SessionState;

pub const CHILD_VFS_RIGHTS: u32 = 0x03;
pub const CHILD_REGISTRY_RIGHTS: u32 = 0x01;
pub const CHILD_TIMESERVER_RIGHTS: u32 = 0x01;

pub struct Spawn;

impl MsgHandler for Spawn {
    const LABEL: u32 = SESSION_PROCMGR_SPAWN_LABEL;
    type State = SessionState;

    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let req: SpawnReq = postcard::from_bytes(msg.payload)
            .map_err(|_| HandlerError::BadPayload)?;

        let pid = state
            .child_table
            .alloc_pid()
            .map_err(|_| HandlerError::Eagain)?;

        // Mint child capabilities under a guard so they're revoked on early return.
        let mut guard = MintGuard::new(&mut state.kernel);
        sub_mint(
            &mut guard,
            state.vfs_cap,
            CapRights(0x07),
            CapRights(CHILD_VFS_RIGHTS),
        )
        .map_err(|_| HandlerError::Internal("vfs"))?;
        sub_mint(
            &mut guard,
            state.registry_cap,
            CapRights(0x03),
            CapRights(CHILD_REGISTRY_RIGHTS),
        )
        .map_err(|_| HandlerError::Internal("registry"))?;
        sub_mint(
            &mut guard,
            state.timeserver_cap,
            CapRights(0x01),
            CapRights(CHILD_TIMESERVER_RIGHTS),
        )
        .map_err(|_| HandlerError::Internal("timeserver"))?;

        // Disarm guard and reclaim handles so we can release the kernel borrow,
        // then call the spawn primitive. On spawn failure we revoke manually.
        let minted: Vec<u64> = guard.forget();

        #[cfg(feature = "host-test")]
        let (thread_tok, cookie, space_tok, child_tid) = {
            let tok = state.kernel.spawn_thread(0xE000_0000, 0xF000_0000);
            if tok == 0 {
                for h in &minted {
                    state.kernel.revoke(*h);
                }
                return Err(HandlerError::Internal("spawn_thread"));
            }
            (tok, (pid as u64) ^ 0xC0DE_0000, 0u64, 0usize)
        };

        #[cfg(not(feature = "host-test"))]
        let (thread_tok, cookie, space_tok, child_tid) = match crate::elf_spawn::real_spawn_user_process(state, pid, &req, msg.sender_tid) {
            Ok(t) => t,
            Err(e) => {
                let _ = libcluu::debug_print(&alloc::format!(
                    "session-procmgr: real_spawn_user_process failed: {:?}", e
                ));
                for h in &minted {
                    state.kernel.revoke(*h);
                }
                return Err(HandlerError::Internal("real_spawn"));
            }
        };
        let local = (pid as u32) & LOCAL_MAX;
        state.child_table.insert(ChildState {
            pid,
            local,
            thread_tok,
            space_tok,
            child_tid,
            cookie,
            argv0: req.argv.first().cloned().unwrap_or_default(),
            start_ticks: 0,
            minted_caps: minted,
            pgid: None,
            notify_ep: req.notify.unwrap_or(0),
        });

        let reply = SpawnReply { pid, cookie };
        let bytes =
            postcard::to_allocvec(&reply).map_err(|_| HandlerError::Internal("postcard"))?;
        Ok(Reply::ok(Self::LABEL).with_payload(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::handler::InboundMsg;
    use procmgr_common::pid::{encode, LOCAL_MAX};
    use procmgr_common::test_kernel::KernelCall;
    use procmgr_common::wire::{FdInheritEntry, FdKind, SpawnReq};
    use crate::dispatch::SessionState;

    fn spawn_req() -> SpawnReq {
        SpawnReq {
            image_path: "/bin/ls".into(),
            argv: alloc::vec!["ls".into(), "-l".into()],
            envp: alloc::vec![],
            cwd: "/".into(),
            fd_inherit: alloc::vec![FdInheritEntry {
                fd: 0,
                kind: FdKind::Pts,
                cap_token: 1,
                parent_rfd: 0,
            }],
            notify: None,
        }
    }

    fn make_msg(payload: &[u8]) -> InboundMsg<'_> {
        InboundMsg {
            label: SESSION_PROCMGR_SPAWN_LABEL,
            words: [0; 6],
            payload,
            sender_tid: 1,
        }
    }

    #[test]
    fn success_path_returns_pid_cookie() {
        let mut state = SessionState::new_for_test(5);
        let req_bytes = postcard::to_allocvec(&spawn_req()).unwrap();
        let msg = make_msg(&req_bytes);
        let reply = Spawn::handle(&mut state, &msg).unwrap();
        let resp: SpawnReply = postcard::from_bytes(&reply.payload).unwrap();
        assert_eq!(resp.pid, encode(5, 1).unwrap());
        // Child must be tracked in the table.
        assert!(state.child_table.lookup_by_pid(resp.pid).is_some());
        // Cookie must match what's stored.
        assert!(state.child_table.lookup_by_cookie(resp.cookie).is_some());
    }

    #[test]
    fn bad_payload_returns_badpayload() {
        let mut state = SessionState::new_for_test(5);
        let msg = make_msg(&[0xFF, 0xFF]);
        let err = Spawn::handle(&mut state, &msg).unwrap_err();
        assert!(matches!(err, HandlerError::BadPayload));
    }

    #[test]
    fn pid_exhausted_returns_eagain() {
        let mut state = SessionState::new_for_test(5);
        // Fill up all but one local pid.
        state.child_table.next_local = LOCAL_MAX;
        // First alloc at LOCAL_MAX should succeed.
        let req_bytes = postcard::to_allocvec(&spawn_req()).unwrap();
        let msg = make_msg(&req_bytes);
        let _ok = Spawn::handle(&mut state, &msg).unwrap();
        // Now next_local > LOCAL_MAX → Eagain.
        let msg2 = make_msg(&req_bytes);
        let err = Spawn::handle(&mut state, &msg2).unwrap_err();
        assert!(matches!(err, HandlerError::Eagain));
    }

    #[test]
    fn sub_mint_records_child_caps() {
        let mut state = SessionState::new_for_test(5);
        let req_bytes = postcard::to_allocvec(&spawn_req()).unwrap();
        let msg = make_msg(&req_bytes);
        Spawn::handle(&mut state, &msg).unwrap();
        let mint_count = state
            .kernel
            .calls
            .iter()
            .filter(|c| matches!(c, KernelCall::Mint { .. }))
            .count();
        assert_eq!(mint_count, 3, "expected exactly 3 minted caps (vfs, registry, timeserver)");
    }

    #[test]
    fn no_orphan_caps_on_thread_spawn_failure() {
        let mut state = SessionState::new_for_test(5);
        state.kernel.fail_next_spawn = true;
        let req_bytes = postcard::to_allocvec(&spawn_req()).unwrap();
        let msg = make_msg(&req_bytes);
        let err = Spawn::handle(&mut state, &msg).unwrap_err();
        assert!(matches!(err, HandlerError::Internal(_)));

        let mint_count = state
            .kernel
            .calls
            .iter()
            .filter(|c| matches!(c, KernelCall::Mint { .. }))
            .count();
        let revoke_count = state
            .kernel
            .calls
            .iter()
            .filter(|c| matches!(c, KernelCall::Revoke { .. }))
            .count();
        assert_eq!(
            mint_count, revoke_count,
            "every minted cap must be revoked on spawn_thread failure: minted={mint_count} revoked={revoke_count}"
        );
    }
}
