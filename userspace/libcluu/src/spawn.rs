//! Client-side unified spawn API.

extern crate alloc;

use core::mem::size_of;

use cluu_proto::spawn::{SpawnEnvelope, SpawnError, SpawnReply, PROCMGR_SPAWN_UNIFIED_LABEL};
use cluu_proto::ABI_VERSION;

use crate::ipc;
use crate::types::Message;

/// CLUU IPC message header size: MessageTag (label u32 + words u8 + extra u8 + pad u16 = 8 bytes)
/// + [usize; 6] words array (6 × 8 = 48 bytes) = 56 bytes total.
const IPC_MSG_HEADER_SIZE: usize = 8 + 6 * core::mem::size_of::<usize>();

/// Maximum reply payload size for spawn responses (SpawnReply or SpawnError).
const SPAWN_REPLY_BUF_SIZE: usize = 512;

/// Issue a unified spawn request to procmgr.
///
/// Serializes the envelope via postcard, sends it via IPC to procmgr,
/// and returns the deserialized reply.
pub fn spawn(envelope: SpawnEnvelope) -> Result<SpawnReply, SpawnError> {
    let payload = postcard::to_allocvec(&envelope)
        .map_err(|_| SpawnError::Internal(0xEBADAB3u32))?;

    let procmgr_ep = crate::registry::lookup_service("procmgr:spawn")
        .ok_or(SpawnError::Internal(0xEBADAB4u32))?;

    let words = [payload.len(), ABI_VERSION as usize, 0, 0, 0, 0];
    let msg = Message::new(PROCMGR_SPAWN_UNIFIED_LABEL, words, 0);

    let mut reply_buf = [0u8; SPAWN_REPLY_BUF_SIZE];
    let (_reply_msg, bytes_received) = ipc::call_with_reply_buf(procmgr_ep, &msg, &payload, &mut reply_buf)
        .map_err(|_| SpawnError::Internal(0xEBADAB5u32))?;

    let reply_payload_start = IPC_MSG_HEADER_SIZE;
    let reply_payload = if bytes_received > reply_payload_start {
        &reply_buf[reply_payload_start..bytes_received]
    } else {
        &[]
    };

    let result: Result<SpawnReply, SpawnError> = postcard::from_bytes(reply_payload)
        .map_err(|_| SpawnError::Internal(0xEBADAB6u32))?;

    result
}