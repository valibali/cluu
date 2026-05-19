//! Client-side wrappers around the procmgr session verbs.

extern crate alloc;

use cluu_proto::ABI_VERSION;
use cluu_proto::TokenHandle;
use cluu_proto::session::*;

use crate::ipc;
use crate::types::Message;

const REPLY_BUF_SIZE: usize = 512;

pub fn create(req: SessionCreateRequest) -> Result<SessionCreateOk, SessionCreateErr> {
    let payload = postcard::to_allocvec(&req)
        .map_err(|_| SessionCreateErr::Internal(0xE0u32))?;

    let procmgr_ep = crate::registry::lookup_service("procmgr:session")
        .ok_or(SessionCreateErr::Internal(0xE1u32))?;

    let words = [payload.len(), ABI_VERSION as usize, 0, 0, 0, 0];
    let msg = Message::new(PROCMGR_SESSION_CREATE_LABEL, words, 0);

    let mut reply_buf = [0u8; REPLY_BUF_SIZE];
    let (_reply_msg, bytes_received) = ipc::call_with_reply_buf(
        procmgr_ep, &msg, &payload, &mut reply_buf)
        .map_err(|_| SessionCreateErr::Internal(0xE2u32))?;

    let reply_payload = if bytes_received > 0 {
        &reply_buf[..bytes_received]
    } else {
        &[]
    };

    postcard::from_bytes::<SessionCreateReply>(reply_payload)
        .map_err(|_| SessionCreateErr::Internal(0xE3u32))?
}

pub fn destroy(token: TokenHandle) -> Result<(), SessionErr> {
    let req = SessionDestroyRequest { token };
    let payload = postcard::to_allocvec(&req)
        .map_err(|_| SessionErr::Internal(0xE4u32))?;

    let procmgr_ep = crate::registry::lookup_service("procmgr:session")
        .ok_or(SessionErr::Internal(0xE5u32))?;

    let words = [payload.len(), ABI_VERSION as usize, 0, 0, 0, 0];
    let msg = Message::new(PROCMGR_SESSION_DESTROY_LABEL, words, 0);

    let mut reply_buf = [0u8; REPLY_BUF_SIZE];
    let (_reply_msg, bytes_received) = ipc::call_with_reply_buf(
        procmgr_ep, &msg, &payload, &mut reply_buf)
        .map_err(|_| SessionErr::Internal(0xE6u32))?;

    let reply_payload = if bytes_received > 0 {
        &reply_buf[..bytes_received]
    } else {
        &[]
    };

    postcard::from_bytes::<SessionDestroyReply>(reply_payload)
        .map_err(|_| SessionErr::Internal(0xE7u32))?
}

pub fn query(token: TokenHandle) -> Result<SessionQueryReply, SessionErr> {
    let req = SessionQueryRequest { token };
    let payload = postcard::to_allocvec(&req)
        .map_err(|_| SessionErr::Internal(0xE8u32))?;

    let procmgr_ep = crate::registry::lookup_service("procmgr:session")
        .ok_or(SessionErr::Internal(0xE9u32))?;

    let words = [payload.len(), ABI_VERSION as usize, 0, 0, 0, 0];
    let msg = Message::new(PROCMGR_SESSION_QUERY_LABEL, words, 0);

    let mut reply_buf = [0u8; REPLY_BUF_SIZE];
    let (_reply_msg, bytes_received) = ipc::call_with_reply_buf(
        procmgr_ep, &msg, &payload, &mut reply_buf)
        .map_err(|_| SessionErr::Internal(0xEAu32))?;

    let reply_payload = if bytes_received > 0 {
        &reply_buf[..bytes_received]
    } else {
        &[]
    };

    postcard::from_bytes::<SessionQueryReply>(reply_payload)
        .map_err(|_| SessionErr::Internal(0xEBu32))
}

pub fn subscribe(token: TokenHandle, event_send: TokenHandle) -> Result<(), SessionErr> {
    let req = SessionSubscribeRequest { token, event_send };
    let payload = postcard::to_allocvec(&req)
        .map_err(|_| SessionErr::Internal(0xECu32))?;

    let procmgr_ep = crate::registry::lookup_service("procmgr:session")
        .ok_or(SessionErr::Internal(0xEDu32))?;

    let words = [payload.len(), ABI_VERSION as usize, 0, 0, 0, 0];
    let msg = Message::new(PROCMGR_SESSION_SUBSCRIBE_LABEL, words, 0);

    let mut reply_buf = [0u8; REPLY_BUF_SIZE];
    let (_reply_msg, bytes_received) = ipc::call_with_reply_buf(
        procmgr_ep, &msg, &payload, &mut reply_buf)
        .map_err(|_| SessionErr::Internal(0xEEu32))?;

    let reply_payload = if bytes_received > 0 {
        &reply_buf[..bytes_received]
    } else {
        &[]
    };

    postcard::from_bytes::<SessionSubscribeReply>(reply_payload)
        .map_err(|_| SessionErr::Internal(0xEFu32))?
}

pub fn derive_token(token: TokenHandle, rights: u32) -> Result<TokenHandle, SessionErr> {
    let req = SessionDeriveRequest { token, rights };
    let payload = postcard::to_allocvec(&req)
        .map_err(|_| SessionErr::Internal(0xF0u32))?;

    let procmgr_ep = crate::registry::lookup_service("procmgr:session")
        .ok_or(SessionErr::Internal(0xF1u32))?;

    let words = [payload.len(), ABI_VERSION as usize, 0, 0, 0, 0];
    let msg = Message::new(PROCMGR_SESSION_DERIVE_TOKEN_LABEL, words, 0);

    let mut reply_buf = [0u8; REPLY_BUF_SIZE];
    let (_reply_msg, bytes_received) = ipc::call_with_reply_buf(
        procmgr_ep, &msg, &payload, &mut reply_buf)
        .map_err(|_| SessionErr::Internal(0xF2u32))?;

    let reply_payload = if bytes_received > 0 {
        &reply_buf[..bytes_received]
    } else {
        &[]
    };

    postcard::from_bytes::<SessionDeriveReply>(reply_payload)
        .map_err(|_| SessionErr::Internal(0xF3u32))?
}

pub fn set_leader(token: TokenHandle, leader_pid: u32) -> Result<(), SessionErr> {
    let req = SessionSetLeaderRequest { token, leader_pid };
    let payload = postcard::to_allocvec(&req)
        .map_err(|_| SessionErr::Internal(0xF4u32))?;

    let procmgr_ep = crate::registry::lookup_service("procmgr:session")
        .ok_or(SessionErr::Internal(0xF5u32))?;

    let words = [payload.len(), ABI_VERSION as usize, 0, 0, 0, 0];
    let msg = Message::new(PROCMGR_SESSION_SET_LEADER_LABEL, words, 0);

    let mut reply_buf = [0u8; REPLY_BUF_SIZE];
    let (_reply_msg, bytes_received) = ipc::call_with_reply_buf(
        procmgr_ep, &msg, &payload, &mut reply_buf)
        .map_err(|_| SessionErr::Internal(0xF6u32))?;

    let reply_payload = if bytes_received > 0 {
        &reply_buf[..bytes_received]
    } else {
        &[]
    };

    postcard::from_bytes::<SessionSetLeaderReply>(reply_payload)
        .map_err(|_| SessionErr::Internal(0xF7u32))?
}