//! Session lifecycle protocol — see spec 3.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::TokenHandle;
use crate::spawn::ViewSource;

// ----- Verb labels -----

pub const PROCMGR_SESSION_CREATE_LABEL:        u32 = 82;
pub const PROCMGR_SESSION_DESTROY_LABEL:       u32 = 83;
pub const PROCMGR_SESSION_QUERY_LABEL:         u32 = 84;
pub const PROCMGR_SESSION_SUBSCRIBE_LABEL:     u32 = 85;
pub const PROCMGR_SESSION_DERIVE_TOKEN_LABEL:  u32 = 86;
pub const SESSION_ENDED_LABEL:                 u32 = 87;   // async event
pub const PROCMGR_SESSION_SET_LEADER_LABEL:    u32 = 88;

// Compositor:control verb (for the login → compositor handoff).
pub const COMPOSITOR_SESSION_HANDOFF_LABEL:    u32 = 200;

// ----- Rights bitmask -----

pub const RIGHT_SESSION_CONTROL:   u32 = 0x01;
pub const RIGHT_SESSION_QUERY:     u32 = 0x02;
pub const RIGHT_SESSION_SUBSCRIBE: u32 = 0x04;
pub const RIGHT_SESSION_JOIN:      u32 = 0x08;

pub const RIGHT_SESSION_ALL: u32 = RIGHT_SESSION_CONTROL
                                 | RIGHT_SESSION_QUERY
                                 | RIGHT_SESSION_SUBSCRIBE
                                 | RIGHT_SESSION_JOIN;

// ----- Errors -----

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionErr {
    InvalidToken,
    InsufficientRights,
    AlreadyDying,
    AlreadyHasLeader,
    LeaderNotMember,
    NotFound,
    Internal(u32),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionCreateErr {
    PermissionDenied,
    InvalidProfile,
    Internal(u32),
}

// ----- Requests / replies -----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileSpec {
    pub home: String,
    pub initial_view: ViewSource,
    pub env: Vec<(String, String)>,
    pub umask: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionCreateRequest {
    pub user_name: String,
    pub profile:   ProfileSpec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionCreateOk {
    pub token:      TokenHandle,
    pub session_id: u32,
}
pub type SessionCreateReply = Result<SessionCreateOk, SessionCreateErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionDestroyRequest { pub token: TokenHandle }
pub type   SessionDestroyReply   = Result<(), SessionErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionQueryRequest  { pub token: TokenHandle }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionState { Live, Dying }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionQueryReply {
    pub session_id:  u32,
    pub user_name:   String,
    pub leader_pid:  Option<u32>,
    pub state:       SessionState,
    pub member_pids: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSubscribeRequest {
    pub token:      TokenHandle,
    pub event_send: TokenHandle,
}
pub type SessionSubscribeReply = Result<(), SessionErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionDeriveRequest { pub token: TokenHandle, pub rights: u32 }
pub type   SessionDeriveReply   = Result<TokenHandle, SessionErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSetLeaderRequest { pub token: TokenHandle, pub leader_pid: u32 }
pub type   SessionSetLeaderReply   = Result<(), SessionErr>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEndedEvent { pub session_id: u32 }

// ----- Compositor handoff -----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompositorSessionHandoffRequest {
    pub session_id: u32,
    pub token_sub:  TokenHandle,
}
pub type CompositorSessionHandoffReply = Result<(), SessionErr>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::ViewSource;
    use alloc::vec;

    fn sample_profile() -> ProfileSpec {
        ProfileSpec {
            home: String::from("/home/dave"),
            initial_view: ViewSource::Derive(0xC0FFEE),
            env: vec![(String::from("USER"), String::from("dave"))],
            umask: 0o022,
        }
    }

    #[test]
    fn create_request_roundtrip() {
        let req = SessionCreateRequest {
            user_name: String::from("dave"),
            profile: sample_profile(),
        };
        let bytes = postcard::to_allocvec(&req).expect("ser");
        let decoded: SessionCreateRequest = postcard::from_bytes(&bytes).expect("deser");
        assert_eq!(decoded.user_name, "dave");
        assert_eq!(decoded.profile.home, "/home/dave");
    }

    #[test]
    fn query_reply_roundtrip() {
        let r = SessionQueryReply {
            session_id: 7,
            user_name: String::from("dave"),
            leader_pid: Some(42),
            state: SessionState::Live,
            member_pids: vec![42, 43, 44],
        };
        let bytes = postcard::to_allocvec(&r).expect("ser");
        let decoded: SessionQueryReply = postcard::from_bytes(&bytes).expect("deser");
        assert_eq!(decoded.session_id, 7);
        assert_eq!(decoded.leader_pid, Some(42));
        assert_eq!(decoded.member_pids.len(), 3);
        assert_eq!(decoded.state, SessionState::Live);
    }

    #[test]
    fn session_ended_event_roundtrip() {
        let e = SessionEndedEvent { session_id: 99 };
        let bytes = postcard::to_allocvec(&e).expect("ser");
        let decoded: SessionEndedEvent = postcard::from_bytes(&bytes).expect("deser");
        assert_eq!(decoded.session_id, 99);
    }

    #[test]
    fn rights_subset_check() {
        let full = RIGHT_SESSION_ALL;
        let qonly = RIGHT_SESSION_QUERY;
        assert_eq!(qonly & full, qonly); // subset
        assert_ne!(full & qonly, full);  // not equal (qonly is narrower)
    }
}