extern crate alloc;
use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::kernel_iface::Kernel;
use procmgr_common::labels::PROCMGR_EXIT_LABEL;
use crate::dispatch::SessionState;

pub struct ChildExit;

impl MsgHandler for ChildExit {
    const LABEL: u32 = PROCMGR_EXIT_LABEL;
    type State = SessionState;

    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let cookie = msg.words[0] as u64;
        let exit_code = msg.words[1] as i32;

        let pid = match state.child_table.lookup_by_cookie(cookie) {
            Some(c) => c.pid,
            None => return Ok(Reply::ok(Self::LABEL)), // drop unknown silently
        };
        let child = state
            .child_table
            .remove(pid)
            .map_err(|_| HandlerError::Internal("remove"))?;

        // 1. Debug log (mirrors root-procmgr PROC_EXIT log).
        #[cfg(not(feature = "host-test"))]
        let _ = libcluu::debug_print(&alloc::format!(
            "procmgr: exit cookie {} (code {})",
            cookie, exit_code
        ));
        // Suppress unused-variable warning in host-test builds.
        let _ = exit_code;

        // 2. Destroy thread.
        state.kernel.thread_destroy(child.thread_tok);
        #[cfg(not(feature = "host-test"))]
        let _ = libcluu::debug_print(&alloc::format!(
            "TRACE: reaped thread token {}",
            child.thread_tok
        ));

        // 3. Destroy address space.
        if child.space_tok != 0 {
            state.kernel.space_destroy(child.space_tok);
        }

        // 4. Revoke all derived tokens/endpoints created for this child
        //    (vfs, registry, timeserver sub-minted caps + any fd-derived tokens).
        for h in child.minted_caps {
            state.kernel.revoke(h);
        }

        // 5. VFS container cleanup: session-procmgr spawns with container_id=0
        //    (hardcoded in elf_spawn VFS_SET_VIEW), so there is no per-child VFS
        //    container to clean up here.  The VFS view is tied to child_tid and
        //    is implicitly dropped when the thread is destroyed.

        // 6. Session-death cascade (HR4): root-procmgr's responsibility only.
        //    Session-procmgr has no VT/tty_endpoints — do not replicate here.

        Ok(Reply::ok(Self::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use procmgr_common::handler::InboundMsg;
    use procmgr_common::test_kernel::KernelCall;
    use crate::child_table::ChildState;

    #[test]
    fn known_cookie_removes_and_revokes() {
        let mut s = SessionState::new_for_test(5);
        let pid = s.child_table.alloc_pid().unwrap();
        s.child_table.insert(ChildState {
            pid,
            local: 1,
            thread_tok: 0x100,
            space_tok: 0x200,
            cookie: 0xC0DE,
            argv0: "ls".into(),
            start_ticks: 0,
            minted_caps: alloc::vec![0xA, 0xB],
            pgid: None,
        });
        let msg = InboundMsg {
            label: ChildExit::LABEL,
            words: [0xC0DE, 0, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        ChildExit::handle(&mut s, &msg).unwrap();
        assert!(s.child_table.lookup_by_pid(pid).is_none());

        // Expect: ThreadDestroy(0x100), SpaceDestroy(0x200), Revoke(0xA), Revoke(0xB)
        let thread_destroys: alloc::vec::Vec<u64> = s
            .kernel
            .calls
            .iter()
            .filter_map(|c| match c {
                KernelCall::ThreadDestroy { thread_tok } => Some(*thread_tok),
                _ => None,
            })
            .collect();
        assert_eq!(thread_destroys, alloc::vec![0x100u64], "expected thread_destroy(0x100)");

        let space_destroys: alloc::vec::Vec<u64> = s
            .kernel
            .calls
            .iter()
            .filter_map(|c| match c {
                KernelCall::SpaceDestroy { space_tok } => Some(*space_tok),
                _ => None,
            })
            .collect();
        assert_eq!(space_destroys, alloc::vec![0x200u64], "expected space_destroy(0x200)");

        let revokes: alloc::vec::Vec<u64> = s
            .kernel
            .calls
            .iter()
            .filter_map(|c| match c {
                KernelCall::Revoke { handle } => Some(*handle),
                _ => None,
            })
            .collect();
        assert_eq!(revokes, alloc::vec![0xA, 0xB]);
    }

    #[test]
    fn zero_space_tok_skips_space_destroy() {
        let mut s = SessionState::new_for_test(5);
        let pid = s.child_table.alloc_pid().unwrap();
        s.child_table.insert(ChildState {
            pid,
            local: 1,
            thread_tok: 0x100,
            space_tok: 0, // mock / no real space
            cookie: 0xC0DE,
            argv0: "sh".into(),
            start_ticks: 0,
            minted_caps: alloc::vec![],
            pgid: None,
        });
        let msg = InboundMsg {
            label: ChildExit::LABEL,
            words: [0xC0DE, 0, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        ChildExit::handle(&mut s, &msg).unwrap();
        assert!(s.child_table.lookup_by_pid(pid).is_none());

        let space_destroys = s
            .kernel
            .calls
            .iter()
            .filter(|c| matches!(c, KernelCall::SpaceDestroy { .. }))
            .count();
        assert_eq!(space_destroys, 0, "space_tok==0 must not call space_destroy");
    }

    #[test]
    fn unknown_cookie_drops_silently() {
        let mut s = SessionState::new_for_test(5);
        let msg = InboundMsg {
            label: ChildExit::LABEL,
            words: [0xDEAD, 0, 0, 0, 0, 0],
            payload: &[],
            sender_tid: 1,
        };
        ChildExit::handle(&mut s, &msg).unwrap();
        assert_eq!(s.kernel.calls.len(), 0);
    }
}
