//! Kernel runtime telemetry counters.
//!
//! This module intentionally keeps telemetry minimal and lock-free so it can be
//! used in hot paths (token and IPC syscalls) with low overhead.

use crate::token::{Issuer, ObjectRef, Token, TokenHandle};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

static TOKENS_CREATED: AtomicU64 = AtomicU64::new(0);
static TOKENS_REVOKED: AtomicU64 = AtomicU64::new(0);
static IPC_RECV_WOULD_BLOCK: AtomicU64 = AtomicU64::new(0);
static IPC_RECV_TIMEOUT: AtomicU64 = AtomicU64::new(0);
static BOOT_TOKEN_GRANTS: AtomicU64 = AtomicU64::new(0);
const TOKEN_AUDIT_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TokenAuditOp {
    Create = 1,
    Derive = 2,
    Revoke = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenAuditEvent {
    pub seq: u64,
    pub op: TokenAuditOp,
    pub handle: u64,
    pub object_kind: u8,
    pub object_id: u64,
    pub issuer_kind: u8,
    pub issuer_id: u64,
    pub rights_bits: u32,
    pub expire_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenAuditStats {
    pub next_seq: u64,
    pub stored: usize,
    pub dropped: u64,
}

struct TokenAuditRing {
    events: [Option<TokenAuditEvent>; TOKEN_AUDIT_CAPACITY],
    next_seq: u64,
    stored: usize,
    dropped: u64,
}

impl TokenAuditRing {
    const fn new() -> Self {
        Self {
            events: [None; TOKEN_AUDIT_CAPACITY],
            next_seq: 1,
            stored: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, mut event: TokenAuditEvent) {
        event.seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);

        let slot = (event.seq as usize - 1) % TOKEN_AUDIT_CAPACITY;
        if self.events[slot].is_some() {
            self.dropped = self.dropped.saturating_add(1);
        } else {
            self.stored = self.stored.saturating_add(1);
        }
        self.events[slot] = Some(event);
    }

    fn stats(&self) -> TokenAuditStats {
        TokenAuditStats {
            next_seq: self.next_seq,
            stored: self.stored,
            dropped: self.dropped,
        }
    }
}

static TOKEN_AUDIT_RING: Mutex<TokenAuditRing> = Mutex::new(TokenAuditRing::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub tokens_created: u64,
    pub tokens_revoked: u64,
    pub ipc_recv_would_block: u64,
    pub ipc_recv_timeout: u64,
    pub boot_token_grants: u64,
    pub token_audit_next_seq: u64,
    pub token_audit_stored: usize,
    pub token_audit_dropped: u64,
}

#[inline(always)]
pub fn record_token_created() {
    TOKENS_CREATED.fetch_add(1, Ordering::Relaxed);
}

#[inline(always)]
pub fn record_token_revoked(count: u64) {
    TOKENS_REVOKED.fetch_add(count, Ordering::Relaxed);
}

#[inline(always)]
pub fn record_ipc_recv_would_block() {
    IPC_RECV_WOULD_BLOCK.fetch_add(1, Ordering::Relaxed);
}

#[inline(always)]
pub fn record_ipc_recv_timeout() {
    IPC_RECV_TIMEOUT.fetch_add(1, Ordering::Relaxed);
}

#[inline(always)]
pub fn record_boot_token_grant() {
    BOOT_TOKEN_GRANTS.fetch_add(1, Ordering::Relaxed);
}

#[inline(always)]
pub fn snapshot() -> Snapshot {
    let audit = token_audit_stats();
    Snapshot {
        tokens_created: TOKENS_CREATED.load(Ordering::Relaxed),
        tokens_revoked: TOKENS_REVOKED.load(Ordering::Relaxed),
        ipc_recv_would_block: IPC_RECV_WOULD_BLOCK.load(Ordering::Relaxed),
        ipc_recv_timeout: IPC_RECV_TIMEOUT.load(Ordering::Relaxed),
        boot_token_grants: BOOT_TOKEN_GRANTS.load(Ordering::Relaxed),
        token_audit_next_seq: audit.next_seq,
        token_audit_stored: audit.stored,
        token_audit_dropped: audit.dropped,
    }
}

#[inline(always)]
fn object_fields(object_ref: ObjectRef) -> (u8, u64) {
    match object_ref {
        ObjectRef::Thread(id) => (1, id.as_u64()),
        ObjectRef::Space(id) => (2, id.as_u64()),
        ObjectRef::Endpoint(id) => (3, id.as_u64()),
        ObjectRef::Irq(irq) => (4, irq as u64),
        ObjectRef::Reply(id) => (5, id.as_u64()),
        ObjectRef::Clock => (6, 0),
        ObjectRef::Frame(id) => (7, id.as_u64()),
    }
}

#[inline(always)]
fn issuer_fields(issuer: Issuer) -> (u8, u64) {
    match issuer {
        Issuer::Kernel => (1, 0),
        Issuer::Authority(id) => (2, id.as_u64()),
    }
}

#[inline(always)]
pub fn record_token_audit_create(
    handle: TokenHandle,
    token: &Token,
    object_ref: ObjectRef,
    derived: bool,
) {
    let (object_kind, object_id) = object_fields(object_ref);
    let (issuer_kind, issuer_id) = issuer_fields(token.issuer);
    TOKEN_AUDIT_RING.lock().push(TokenAuditEvent {
        seq: 0,
        op: if derived {
            TokenAuditOp::Derive
        } else {
            TokenAuditOp::Create
        },
        handle: handle.as_raw() as u64,
        object_kind,
        object_id,
        issuer_kind,
        issuer_id,
        rights_bits: token.role.bits(),
        expire_at: token.expire_at.as_u64(),
    });
}

#[inline(always)]
pub fn record_token_audit_revoke(
    handle: TokenHandle,
    token: Option<&Token>,
    object_ref: Option<ObjectRef>,
) {
    let (object_kind, object_id) = object_ref.map(object_fields).unwrap_or((0, 0));
    let (issuer_kind, issuer_id, rights_bits, expire_at) = match token {
        Some(t) => {
            let (kind, id) = issuer_fields(t.issuer);
            (kind, id, t.role.bits(), t.expire_at.as_u64())
        }
        None => (0, 0, 0, 0),
    };
    TOKEN_AUDIT_RING.lock().push(TokenAuditEvent {
        seq: 0,
        op: TokenAuditOp::Revoke,
        handle: handle.as_raw() as u64,
        object_kind,
        object_id,
        issuer_kind,
        issuer_id,
        rights_bits,
        expire_at,
    });
}

pub fn token_audit_stats() -> TokenAuditStats {
    TOKEN_AUDIT_RING.lock().stats()
}

pub fn log_bootstrap_snapshot(stage: &str) {
    let s = snapshot();

    klibcluu::info("telemetry snapshot: ");
    klibcluu::info(stage);

    klibcluu::info("  tokens_created=");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", s.tokens_created);

    klibcluu::info("  tokens_revoked=");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", s.tokens_revoked);

    klibcluu::info("  ipc_recv_would_block=");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", s.ipc_recv_would_block);

    klibcluu::info("  ipc_recv_timeout=");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", s.ipc_recv_timeout);

    klibcluu::info("  boot_token_grants=");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", s.boot_token_grants);

    klibcluu::info("  token_audit_next_seq=");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", s.token_audit_next_seq);

    klibcluu::info("  token_audit_stored=");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", s.token_audit_stored as u64);

    klibcluu::info("  token_audit_dropped=");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", s.token_audit_dropped);
}

#[cfg(test)]
pub fn reset_for_tests() {
    TOKENS_CREATED.store(0, Ordering::Relaxed);
    TOKENS_REVOKED.store(0, Ordering::Relaxed);
    IPC_RECV_WOULD_BLOCK.store(0, Ordering::Relaxed);
    IPC_RECV_TIMEOUT.store(0, Ordering::Relaxed);
    BOOT_TOKEN_GRANTS.store(0, Ordering::Relaxed);
    let mut audit = TOKEN_AUDIT_RING.lock();
    *audit = TokenAuditRing::new();
}
