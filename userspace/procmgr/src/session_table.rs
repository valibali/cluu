//! Procmgr-owned SessionObject table.
//!
//! See spec 3 §4. Every session is a typed object addressed by IPC token.
//! Rights bitmask controls per-token capability.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use cluu_proto::session::{ProfileSpec, RIGHT_SESSION_ALL, SessionErr, SessionState};
use cluu_proto::TokenHandle;

pub type SessionId = u32;

#[derive(Clone, Debug)]
pub struct SessionObject {
    pub id:           SessionId,
    pub user_name:    String,
    pub profile:      ProfileSpec,
    pub creator_pid:  u32,
    pub leader_pid:   Option<u32>,
    pub state:        SessionState,
    pub refcount:     u32,
    pub subscribers:  Vec<Subscriber>,
    pub created_at:   u64,
}

#[derive(Clone, Debug)]
pub struct Subscriber {
    pub event_send_cap: TokenHandle,
    pub owner_pid:      u32,
}

/// Per-token state: which session, what rights, who owns it.
#[derive(Clone, Debug)]
pub struct SessionTokenEntry {
    pub session_id: SessionId,
    pub rights:     u32,
    pub owner_pid:  u32,
}

pub struct SessionTable {
    inner: Mutex<SessionTableInner>,
}

struct SessionTableInner {
    next_session_id: SessionId,
    next_token: u64,
    sessions: BTreeMap<SessionId, SessionObject>,
    /// Token → (session_id, rights, owner_pid)
    tokens: BTreeMap<TokenHandle, SessionTokenEntry>,
}

impl SessionTable {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(SessionTableInner {
                next_session_id: 1,
                next_token: 0xC0DE_0000_0000_0001,
                sessions: BTreeMap::new(),
                tokens: BTreeMap::new(),
            }),
        }
    }

    pub fn create(&self, user_name: String, profile: ProfileSpec, creator_pid: u32, now_ticks: u64)
        -> (SessionId, TokenHandle)
    {
        let mut g = self.inner.lock();
        let id = g.next_session_id;
        g.next_session_id = g.next_session_id.wrapping_add(1);
        let session = SessionObject {
            id, user_name, profile, creator_pid,
            leader_pid: None, state: SessionState::Live,
            refcount: 1, subscribers: Vec::new(),
            created_at: now_ticks,
        };
        g.sessions.insert(id, session);
        let token = g.next_token;
        g.next_token = g.next_token.wrapping_add(1);
        g.tokens.insert(token, SessionTokenEntry {
            session_id: id, rights: RIGHT_SESSION_ALL, owner_pid: creator_pid,
        });
        (id, token)
    }

    pub fn resolve(&self, token: TokenHandle, caller_pid: u32, required_rights: u32)
        -> Result<(SessionId, u32 /* rights */), SessionErr>
    {
        let g = self.inner.lock();
        let entry = g.tokens.get(&token).ok_or(SessionErr::InvalidToken)?;
        if entry.owner_pid != caller_pid {
            return Err(SessionErr::InvalidToken);
        }
        if (entry.rights & required_rights) != required_rights {
            return Err(SessionErr::InsufficientRights);
        }
        Ok((entry.session_id, entry.rights))
    }

    pub fn derive_token(
        &self, parent_token: TokenHandle, caller_pid: u32, requested_rights: u32,
        recipient_pid: u32,
    ) -> Result<TokenHandle, SessionErr> {
        let mut g = self.inner.lock();
        let entry = g.tokens.get(&parent_token).ok_or(SessionErr::InvalidToken)?.clone();
        if entry.owner_pid != caller_pid {
            return Err(SessionErr::InvalidToken);
        }
        if (entry.rights & requested_rights) != requested_rights {
            return Err(SessionErr::InsufficientRights);
        }
        let new_token = g.next_token;
        g.next_token = g.next_token.wrapping_add(1);
        g.tokens.insert(new_token, SessionTokenEntry {
            session_id: entry.session_id,
            rights:     requested_rights,
            owner_pid:  recipient_pid,
        });
        // Bump session refcount.
        if let Some(s) = g.sessions.get_mut(&entry.session_id) {
            s.refcount = s.refcount.saturating_add(1);
        }
        Ok(new_token)
    }

    pub fn set_leader(&self, token: TokenHandle, caller_pid: u32, leader_pid: u32,
                      check_member: impl Fn(u32, SessionId) -> bool)
        -> Result<(), SessionErr>
    {
        // First resolve.
        let session_id = {
            let g = self.inner.lock();
            let entry = g.tokens.get(&token).ok_or(SessionErr::InvalidToken)?;
            if entry.owner_pid != caller_pid {
                return Err(SessionErr::InvalidToken);
            }
            if (entry.rights & cluu_proto::session::RIGHT_SESSION_CONTROL) == 0 {
                return Err(SessionErr::InsufficientRights);
            }
            entry.session_id
        };
        if !check_member(leader_pid, session_id) {
            return Err(SessionErr::LeaderNotMember);
        }
        let mut g = self.inner.lock();
        let session = g.sessions.get_mut(&session_id).ok_or(SessionErr::NotFound)?;
        if session.leader_pid.is_some() {
            return Err(SessionErr::AlreadyHasLeader);
        }
        session.leader_pid = Some(leader_pid);
        Ok(())
    }

    pub fn subscribe(&self, token: TokenHandle, caller_pid: u32, event_send: TokenHandle)
        -> Result<(), SessionErr>
    {
        let session_id = {
            let g = self.inner.lock();
            let entry = g.tokens.get(&token).ok_or(SessionErr::InvalidToken)?;
            if entry.owner_pid != caller_pid {
                return Err(SessionErr::InvalidToken);
            }
            if (entry.rights & cluu_proto::session::RIGHT_SESSION_SUBSCRIBE) == 0 {
                return Err(SessionErr::InsufficientRights);
            }
            entry.session_id
        };
        let mut g = self.inner.lock();
        let session = g.sessions.get_mut(&session_id).ok_or(SessionErr::NotFound)?;
        session.subscribers.push(Subscriber { event_send_cap: event_send, owner_pid: caller_pid });
        session.refcount = session.refcount.saturating_add(1);
        Ok(())
    }

    pub fn snapshot(&self, session_id: SessionId) -> Option<SessionObject> {
        self.inner.lock().sessions.get(&session_id).cloned()
    }

    pub fn mark_dying(&self, session_id: SessionId) -> Option<Vec<Subscriber>> {
        let mut g = self.inner.lock();
        let session = g.sessions.get_mut(&session_id)?;
        if session.state == SessionState::Dying {
            return Some(Vec::new());
        }
        session.state = SessionState::Dying;
        Some(session.subscribers.clone())
    }

    pub fn remove_if_unref(&self, session_id: SessionId) {
        let mut g = self.inner.lock();
        if let Some(s) = g.sessions.get(&session_id) {
            if s.refcount == 0 && s.state == SessionState::Dying {
                g.sessions.remove(&session_id);
            }
        }
    }

    /// On token-owner exit: drop all tokens owned by the dying pid and
    /// decrement the corresponding sessions' refcounts.
    pub fn on_pid_exit(&self, dead_pid: u32) -> Vec<SessionId> {
        let mut g = self.inner.lock();
        let dead_tokens: Vec<TokenHandle> = g.tokens.iter()
            .filter_map(|(t, e)| if e.owner_pid == dead_pid { Some(*t) } else { None })
            .collect();
        let mut affected = Vec::new();
        for t in dead_tokens {
            if let Some(entry) = g.tokens.remove(&t) {
                if let Some(s) = g.sessions.get_mut(&entry.session_id) {
                    s.refcount = s.refcount.saturating_sub(1);
                }
                affected.push(entry.session_id);
            }
        }
        affected
    }
}

pub static SESSION_TABLE: SessionTable = SessionTable::new();