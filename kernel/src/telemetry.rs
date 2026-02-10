//! Kernel runtime telemetry counters.
//!
//! This module intentionally keeps telemetry minimal and lock-free so it can be
//! used in hot paths (token and IPC syscalls) with low overhead.

use core::sync::atomic::{AtomicU64, Ordering};

static TOKENS_CREATED: AtomicU64 = AtomicU64::new(0);
static TOKENS_REVOKED: AtomicU64 = AtomicU64::new(0);
static IPC_RECV_WOULD_BLOCK: AtomicU64 = AtomicU64::new(0);
static IPC_RECV_TIMEOUT: AtomicU64 = AtomicU64::new(0);
static BOOT_TOKEN_GRANTS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub tokens_created: u64,
    pub tokens_revoked: u64,
    pub ipc_recv_would_block: u64,
    pub ipc_recv_timeout: u64,
    pub boot_token_grants: u64,
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
    Snapshot {
        tokens_created: TOKENS_CREATED.load(Ordering::Relaxed),
        tokens_revoked: TOKENS_REVOKED.load(Ordering::Relaxed),
        ipc_recv_would_block: IPC_RECV_WOULD_BLOCK.load(Ordering::Relaxed),
        ipc_recv_timeout: IPC_RECV_TIMEOUT.load(Ordering::Relaxed),
        boot_token_grants: BOOT_TOKEN_GRANTS.load(Ordering::Relaxed),
    }
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
}

#[cfg(test)]
pub fn reset_for_tests() {
    TOKENS_CREATED.store(0, Ordering::Relaxed);
    TOKENS_REVOKED.store(0, Ordering::Relaxed);
    IPC_RECV_WOULD_BLOCK.store(0, Ordering::Relaxed);
    IPC_RECV_TIMEOUT.store(0, Ordering::Relaxed);
    BOOT_TOKEN_GRANTS.store(0, Ordering::Relaxed);
}
