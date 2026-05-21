//! Per-session bookkeeping owned by root-procmgr.
//! Holds session_id allocator (8-bit + generation counter), session metadata,
//! and the spawn endpoint to talk to each session-procmgr instance.

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use procmgr_common::pid::SessionId;

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub sid: SessionId,
    pub generation: u32,
    pub user_name: String,
    pub session_pmgr_thread_tok: u64,
    pub session_pmgr_spawn_ep: u64,
    pub minted_caps: Vec<u64>,      // every cap root minted for this session
    pub state: SessionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState { Live, Dying, Dead }

pub struct SessionDirectory {
    /// generations[sid] = next generation to use when reallocating that sid.
    generations: [u32; 256],
    sessions: BTreeMap<SessionId, SessionEntry>,
    free_stack: Vec<SessionId>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DirError { Exhausted, NotFound, AlreadyDead }

impl Default for SessionDirectory {
    fn default() -> Self { Self::new() }
}

impl SessionDirectory {
    pub fn new() -> Self {
        let mut free = Vec::new();
        for i in (0..=255u8).rev() { free.push(i); }
        Self {
            generations: [0; 256],
            sessions: BTreeMap::new(),
            free_stack: free,
        }
    }

    pub fn alloc_sid(&mut self) -> Result<(SessionId, u32), DirError> {
        let sid = self.free_stack.pop().ok_or(DirError::Exhausted)?;
        let gen = self.generations[sid as usize];
        Ok((sid, gen))
    }

    pub fn insert(&mut self, entry: SessionEntry) {
        self.sessions.insert(entry.sid, entry);
    }

    pub fn lookup(&self, sid: SessionId) -> Option<&SessionEntry> {
        self.sessions.get(&sid)
    }

    pub fn mark_dying(&mut self, sid: SessionId) -> Result<(), DirError> {
        let entry = self.sessions.get_mut(&sid).ok_or(DirError::NotFound)?;
        if entry.state == SessionState::Dead {
            return Err(DirError::AlreadyDead);
        }
        entry.state = SessionState::Dying;
        Ok(())
    }

    /// Mark dead, bump generation, free sid for reuse.
    pub fn finalise_dead(&mut self, sid: SessionId) -> Result<Vec<u64>, DirError> {
        let entry = self.sessions.remove(&sid).ok_or(DirError::NotFound)?;
        self.generations[sid as usize] = self.generations[sid as usize].wrapping_add(1);
        self.free_stack.push(sid);
        Ok(entry.minted_caps)
    }

    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = &SessionEntry> {
        self.sessions.values()
    }
}

// ─── SessionCreate handler ──────────────────────────────────────────────────

use procmgr_common::handler::{HandlerError, InboundMsg, MsgHandler, Reply};
use procmgr_common::labels::PROCMGR_SESSION_CREATE_LABEL;
use procmgr_common::wire::SessionEnvelope;

pub struct SessionCreate;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct SessionCreateReq {
    pub user_name: alloc::string::String,
    pub profile: alloc::string::String,
    pub env_defaults: alloc::vec::Vec<(alloc::string::String, alloc::string::String)>,
    pub view_spec: alloc::string::String,
}

impl MsgHandler for SessionCreate {
    const LABEL: u32 = PROCMGR_SESSION_CREATE_LABEL;
    type State = crate::dispatch::ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let req: SessionCreateReq = postcard::from_bytes(msg.payload)
            .map_err(|_| HandlerError::BadPayload)?;
        let (sid, gen) = state.session_directory.alloc_sid()
            .map_err(|_| HandlerError::Eagain)?;
        let envelope = SessionEnvelope {
            sid, generation: gen,
            user_name: req.user_name.clone(),
            profile: req.profile.clone(),
            pid_base: (sid as i32) << procmgr_common::pid::LOCAL_BITS,
            caps: Vec::new(),  // cap_broker integration in Task 4.2
            env_defaults: req.env_defaults.clone(),
            view_spec: req.view_spec.clone(),
        };
        // session-procmgr spawn deferred to Task 5.1; use placeholder tok/ep.
        let (pmgr_tid, pmgr_ep) = (0u64, 0u64);
        state.session_directory.insert(SessionEntry {
            sid, generation: gen,
            user_name: req.user_name,
            session_pmgr_thread_tok: pmgr_tid,
            session_pmgr_spawn_ep: pmgr_ep,
            minted_caps: Vec::new(),
            state: SessionState::Live,
        });
        let bytes = postcard::to_allocvec(&envelope)
            .map_err(|_| HandlerError::Internal("postcard"))?;
        Ok(Reply::ok(Self::LABEL).with_payload(bytes))
    }
}

// ─── SessionDestroy handler ─────────────────────────────────────────────────

use procmgr_common::kernel_iface::Kernel;
use procmgr_common::labels::PROCMGR_SESSION_DESTROY_LABEL;

pub struct SessionDestroy;

impl MsgHandler for SessionDestroy {
    const LABEL: u32 = PROCMGR_SESSION_DESTROY_LABEL;
    type State = crate::dispatch::ProcmgrState;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
        let sid = msg.words[0] as u8;
        state.session_directory.mark_dying(sid).map_err(|_| HandlerError::NotFound)?;
        let caps = state.session_directory.finalise_dead(sid).map_err(|_| HandlerError::NotFound)?;
        for h in caps {
            state.kernel.revoke(h);
        }
        Ok(Reply::ok(Self::LABEL))
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn entry(sid: SessionId, gen: u32) -> SessionEntry {
        SessionEntry {
            sid, generation: gen,
            user_name: "alice".into(),
            session_pmgr_thread_tok: 0x100,
            session_pmgr_spawn_ep: 0x200,
            minted_caps: alloc::vec![0xA, 0xB, 0xC],
            state: SessionState::Live,
        }
    }

    #[test]
    fn alloc_sid_starts_at_zero_or_predictable() {
        let mut d = SessionDirectory::new();
        let (s, g) = d.alloc_sid().unwrap();
        assert_eq!(g, 0);
        assert!((0..=255u8).contains(&s));
    }

    #[test]
    fn destroy_bumps_generation_and_returns_caps() {
        let mut d = SessionDirectory::new();
        let (s, g) = d.alloc_sid().unwrap();
        d.insert(entry(s, g));
        d.mark_dying(s).unwrap();
        let caps = d.finalise_dead(s).unwrap();
        assert_eq!(caps, alloc::vec![0xA, 0xB, 0xC]);
        let (s2, g2) = d.alloc_sid().unwrap();
        if s2 == s {
            assert_eq!(g2, g.wrapping_add(1));
        }
    }

    #[test]
    fn exhaustion_returns_err() {
        let mut d = SessionDirectory::new();
        for _ in 0..256 { d.alloc_sid().unwrap(); }
        assert_eq!(d.alloc_sid(), Err(DirError::Exhausted));
    }

    #[test]
    fn mark_dying_unknown_sid_errors() {
        let mut d = SessionDirectory::new();
        assert_eq!(d.mark_dying(42), Err(DirError::NotFound));
    }

    proptest! {
        #[test]
        fn prop_no_duplicate_live_sids(ops in proptest::collection::vec(0u8..2, 1..100)) {
            let mut d = SessionDirectory::new();
            let mut held: Vec<(SessionId, u32)> = Vec::new();
            for op in ops {
                if op == 0 {
                    if let Ok((s, g)) = d.alloc_sid() {
                        prop_assert!(!held.iter().any(|(h, _)| *h == s));
                        d.insert(entry(s, g));
                        held.push((s, g));
                    }
                } else if !held.is_empty() {
                    let (s, _) = held.remove(0);
                    d.mark_dying(s).unwrap();
                    d.finalise_dead(s).unwrap();
                }
            }
        }
    }
}

#[cfg(test)]
mod handler_tests {
    use super::*;

    #[test]
    fn create_returns_envelope_with_pid_base() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        let req = SessionCreateReq {
            user_name: "alice".into(), profile: "user".into(),
            env_defaults: alloc::vec![], view_spec: "default".into(),
        };
        let payload = postcard::to_allocvec(&req).unwrap();
        let msg = InboundMsg {
            label: SessionCreate::LABEL, words: [0; 6],
            payload: &payload, sender_tid: 1,
        };
        let reply = SessionCreate::handle(&mut state, &msg).unwrap();
        let env: SessionEnvelope = postcard::from_bytes(&reply.payload).unwrap();
        assert_eq!(env.user_name, "alice");
        assert_eq!(env.generation, 0);
        assert_eq!(env.pid_base, (env.sid as i32) << 23);
    }

    #[test]
    fn create_bad_payload_returns_badpayload() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        let msg = InboundMsg {
            label: SessionCreate::LABEL, words: [0; 6],
            payload: &[0xFF, 0xFF], sender_tid: 1,
        };
        assert!(matches!(SessionCreate::handle(&mut state, &msg), Err(HandlerError::BadPayload)));
    }

    #[test]
    fn create_exhausted_returns_eagain() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        for _ in 0..256 { state.session_directory.alloc_sid().unwrap(); }
        let req = SessionCreateReq {
            user_name: "alice".into(), profile: "u".into(),
            env_defaults: alloc::vec![], view_spec: "default".into(),
        };
        let payload = postcard::to_allocvec(&req).unwrap();
        let msg = InboundMsg {
            label: SessionCreate::LABEL, words: [0; 6],
            payload: &payload, sender_tid: 1,
        };
        assert!(matches!(SessionCreate::handle(&mut state, &msg), Err(HandlerError::Eagain)));
    }
}

#[cfg(test)]
mod destroy_tests {
    use super::*;
    use procmgr_common::test_kernel::KernelCall;

    #[test]
    fn destroy_revokes_all_minted_caps() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        let (s, g) = state.session_directory.alloc_sid().unwrap();
        state.session_directory.insert(SessionEntry {
            sid: s, generation: g, user_name: "alice".into(),
            session_pmgr_thread_tok: 0x100, session_pmgr_spawn_ep: 0x200,
            minted_caps: alloc::vec![0xA, 0xB, 0xC],
            state: SessionState::Live,
        });
        let msg = InboundMsg {
            label: SessionDestroy::LABEL,
            words: [s as usize, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        SessionDestroy::handle(&mut state, &msg).unwrap();
        let revokes: Vec<u64> = state.kernel.calls.iter().filter_map(|c| match c {
            KernelCall::Revoke { handle } => Some(*handle), _ => None,
        }).collect();
        assert_eq!(revokes, alloc::vec![0xA, 0xB, 0xC]);
    }

    #[test]
    fn destroy_unknown_sid_returns_notfound() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        let msg = InboundMsg {
            label: SessionDestroy::LABEL,
            words: [99, 0, 0, 0, 0, 0],
            payload: &[], sender_tid: 1,
        };
        assert!(matches!(SessionDestroy::handle(&mut state, &msg), Err(HandlerError::NotFound)));
    }

    #[test]
    fn destroy_bumps_generation_for_sid_reuse() {
        let mut state = crate::dispatch::ProcmgrState::new_for_test();
        let (s, g0) = state.session_directory.alloc_sid().unwrap();
        state.session_directory.insert(SessionEntry {
            sid: s, generation: g0, user_name: "u".into(),
            session_pmgr_thread_tok: 1, session_pmgr_spawn_ep: 2,
            minted_caps: alloc::vec![], state: SessionState::Live,
        });
        SessionDestroy::handle(&mut state, &InboundMsg {
            label: 0, words: [s as usize, 0, 0, 0, 0, 0], payload: &[], sender_tid: 1,
        }).unwrap();
        let (s2, g1) = state.session_directory.alloc_sid().unwrap();
        assert_eq!(s2, s);
        assert_eq!(g1, g0.wrapping_add(1));
    }
}
