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
static IPC_RECV_WAIT_EVENTS: AtomicU64 = AtomicU64::new(0);
static IPC_RECV_WAIT_TOTAL_TICKS: AtomicU64 = AtomicU64::new(0);
static IPC_RECV_WAIT_MAX_TICKS: AtomicU64 = AtomicU64::new(0);
static IPC_RECV_SCAN_EVENTS: AtomicU64 = AtomicU64::new(0);
static IPC_RECV_SCAN_TOTAL_STEPS: AtomicU64 = AtomicU64::new(0);
static IPC_RECV_SCAN_MAX_STEPS: AtomicU64 = AtomicU64::new(0);
static IPC_STALE_WAITERS: AtomicU64 = AtomicU64::new(0);
static IPC_DIRECT_DELIVERIES: AtomicU64 = AtomicU64::new(0);
static IPC_QUEUE_CUR_BYTES: AtomicU64 = AtomicU64::new(0);
static IPC_QUEUE_PEAK_BYTES: AtomicU64 = AtomicU64::new(0);
static IPC_QUEUE_CUR_MESSAGES: AtomicU64 = AtomicU64::new(0);
static IPC_QUEUE_PEAK_MESSAGES: AtomicU64 = AtomicU64::new(0);
static BOOT_TOKEN_GRANTS: AtomicU64 = AtomicU64::new(0);
static THREAD_SUSPEND_CALLS: AtomicU64 = AtomicU64::new(0);
static THREAD_SUSPEND_SUCCESS: AtomicU64 = AtomicU64::new(0);
static THREAD_RESUME_CALLS: AtomicU64 = AtomicU64::new(0);
static THREAD_RESUME_SUCCESS: AtomicU64 = AtomicU64::new(0);
static RESOURCE_DELTA_LOG_SEQ: AtomicU64 = AtomicU64::new(0);
const TOKEN_AUDIT_CAPACITY: usize = 256;
const IPC_WAIT_HIST_BUCKETS: usize = 12;
// 250Hz timer => 4ms/tick.
const MS_PER_TICK: u64 = 4;
const IPC_WAIT_BUCKET_UPPER_MS: [u64; IPC_WAIT_HIST_BUCKETS] =
    [4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, u64::MAX];
static IPC_RECV_WAIT_HISTOGRAM: [AtomicU64; IPC_WAIT_HIST_BUCKETS] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

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
static RESOURCE_BASELINE: Mutex<Option<ResourceSnapshot>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceSnapshot {
    pub threads_total: u64,
    pub threads_live: u64,
    pub spaces: u64,
    pub endpoints: u64,
    pub tokens: u64,
    pub tracked_frames: u64,
    pub mapped_frames: u64,
    pub pmm_used_frames: u64,
    pub pmm_total_frames: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub tokens_created: u64,
    pub tokens_revoked: u64,
    pub ipc_recv_would_block: u64,
    pub ipc_recv_timeout: u64,
    pub ipc_recv_wait_events: u64,
    pub ipc_recv_wait_avg_ms: u64,
    pub ipc_recv_wait_max_ms: u64,
    pub ipc_recv_wait_p95_ms: u64,
    pub ipc_recv_wait_p99_ms: u64,
    pub ipc_recv_scan_events: u64,
    pub ipc_recv_scan_avg_steps_x100: u64,
    pub ipc_recv_scan_max_steps: u64,
    pub ipc_stale_waiters: u64,
    pub ipc_direct_deliveries: u64,
    pub ipc_queue_bytes_current: u64,
    pub ipc_queue_bytes_peak: u64,
    pub ipc_queue_messages_current: u64,
    pub ipc_queue_messages_peak: u64,
    pub boot_token_grants: u64,
    pub thread_suspend_calls: u64,
    pub thread_suspend_success: u64,
    pub thread_resume_calls: u64,
    pub thread_resume_success: u64,
    pub token_audit_next_seq: u64,
    pub token_audit_stored: usize,
    pub token_audit_dropped: u64,
    pub resources: ResourceSnapshot,
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
fn update_max_atomic(target: &AtomicU64, candidate: u64) {
    let mut current = target.load(Ordering::Relaxed);
    while candidate > current {
        match target.compare_exchange(current, candidate, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

#[inline(always)]
fn wait_histogram_bucket_index(wait_ms: u64) -> usize {
    for (idx, upper) in IPC_WAIT_BUCKET_UPPER_MS.iter().enumerate() {
        if wait_ms <= *upper {
            return idx;
        }
    }
    IPC_WAIT_HIST_BUCKETS - 1
}

#[inline(always)]
pub fn record_ipc_recv_wait_ticks(wait_ticks: u64) {
    let wait_ms = wait_ticks.saturating_mul(MS_PER_TICK);
    IPC_RECV_WAIT_EVENTS.fetch_add(1, Ordering::Relaxed);
    IPC_RECV_WAIT_TOTAL_TICKS.fetch_add(wait_ticks, Ordering::Relaxed);
    update_max_atomic(&IPC_RECV_WAIT_MAX_TICKS, wait_ticks);
    let bucket = wait_histogram_bucket_index(wait_ms);
    IPC_RECV_WAIT_HISTOGRAM[bucket].fetch_add(1, Ordering::Relaxed);
}

#[inline(always)]
pub fn record_ipc_recv_scan_steps(steps: usize) {
    let steps_u64 = steps as u64;
    IPC_RECV_SCAN_EVENTS.fetch_add(1, Ordering::Relaxed);
    IPC_RECV_SCAN_TOTAL_STEPS.fetch_add(steps_u64, Ordering::Relaxed);
    update_max_atomic(&IPC_RECV_SCAN_MAX_STEPS, steps_u64);
}

#[inline(always)]
pub fn record_ipc_stale_waiter() {
    IPC_STALE_WAITERS.fetch_add(1, Ordering::Relaxed);
}

#[inline(always)]
pub fn record_boot_token_grant() {
    BOOT_TOKEN_GRANTS.fetch_add(1, Ordering::Relaxed);
}

#[inline(always)]
pub fn record_ipc_direct_delivery() {
    IPC_DIRECT_DELIVERIES.fetch_add(1, Ordering::Relaxed);
}

#[inline(always)]
fn saturating_sub_atomic(target: &AtomicU64, amount: u64) {
    let mut current = target.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(amount);
        match target.compare_exchange(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

#[inline(always)]
pub fn record_ipc_queue_enqueue(bytes: usize) {
    let added_bytes = bytes as u64;
    let current_bytes = IPC_QUEUE_CUR_BYTES
        .fetch_add(added_bytes, Ordering::Relaxed)
        .saturating_add(added_bytes);
    update_max_atomic(&IPC_QUEUE_PEAK_BYTES, current_bytes);

    let current_messages = IPC_QUEUE_CUR_MESSAGES
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    update_max_atomic(&IPC_QUEUE_PEAK_MESSAGES, current_messages);

    // Trip-wire: when total queued messages crosses a power-of-2 high
    // water mark above 32, dump per-endpoint depths so we can identify
    // *which* endpoint is clogging up.  Power-of-2 spacing keeps the
    // log noise bounded under sustained load (32, 64, 128, 256, ...).
    if current_messages >= 32 && current_messages.is_power_of_two() {
        crate::ipc::endpoint::dump_heavy_endpoints(8);
    }
}

#[inline(always)]
pub fn record_ipc_queue_dequeue(bytes: usize) {
    saturating_sub_atomic(&IPC_QUEUE_CUR_BYTES, bytes as u64);
    saturating_sub_atomic(&IPC_QUEUE_CUR_MESSAGES, 1);
}

#[inline(always)]
pub fn record_thread_suspend(success: bool) {
    THREAD_SUSPEND_CALLS.fetch_add(1, Ordering::Relaxed);
    if success {
        THREAD_SUSPEND_SUCCESS.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline(always)]
pub fn record_thread_resume(success: bool) {
    THREAD_RESUME_CALLS.fetch_add(1, Ordering::Relaxed);
    if success {
        THREAD_RESUME_SUCCESS.fetch_add(1, Ordering::Relaxed);
    }
}

fn wait_percentile_ms(events: u64, percentile: u64) -> u64 {
    if events == 0 {
        return 0;
    }
    let rank = events
        .saturating_mul(percentile)
        .saturating_add(99)
        .saturating_div(100);
    let mut cumulative = 0u64;
    for (idx, bucket) in IPC_RECV_WAIT_HISTOGRAM.iter().enumerate() {
        cumulative = cumulative.saturating_add(bucket.load(Ordering::Relaxed));
        if cumulative >= rank {
            return IPC_WAIT_BUCKET_UPPER_MS[idx];
        }
    }
    IPC_WAIT_BUCKET_UPPER_MS[IPC_WAIT_HIST_BUCKETS - 1]
}

#[inline(always)]
pub fn snapshot() -> Snapshot {
    let audit = token_audit_stats();
    let wait_events = IPC_RECV_WAIT_EVENTS.load(Ordering::Relaxed);
    let wait_total_ticks = IPC_RECV_WAIT_TOTAL_TICKS.load(Ordering::Relaxed);
    let wait_max_ticks = IPC_RECV_WAIT_MAX_TICKS.load(Ordering::Relaxed);
    let scan_events = IPC_RECV_SCAN_EVENTS.load(Ordering::Relaxed);
    let scan_total_steps = IPC_RECV_SCAN_TOTAL_STEPS.load(Ordering::Relaxed);
    Snapshot {
        tokens_created: TOKENS_CREATED.load(Ordering::Relaxed),
        tokens_revoked: TOKENS_REVOKED.load(Ordering::Relaxed),
        ipc_recv_would_block: IPC_RECV_WOULD_BLOCK.load(Ordering::Relaxed),
        ipc_recv_timeout: IPC_RECV_TIMEOUT.load(Ordering::Relaxed),
        ipc_recv_wait_events: wait_events,
        ipc_recv_wait_avg_ms: if wait_events == 0 {
            0
        } else {
            wait_total_ticks.saturating_mul(MS_PER_TICK) / wait_events
        },
        ipc_recv_wait_max_ms: wait_max_ticks.saturating_mul(MS_PER_TICK),
        ipc_recv_wait_p95_ms: wait_percentile_ms(wait_events, 95),
        ipc_recv_wait_p99_ms: wait_percentile_ms(wait_events, 99),
        ipc_recv_scan_events: scan_events,
        ipc_recv_scan_avg_steps_x100: if scan_events == 0 {
            0
        } else {
            scan_total_steps.saturating_mul(100) / scan_events
        },
        ipc_recv_scan_max_steps: IPC_RECV_SCAN_MAX_STEPS.load(Ordering::Relaxed),
        ipc_stale_waiters: IPC_STALE_WAITERS.load(Ordering::Relaxed),
        ipc_direct_deliveries: IPC_DIRECT_DELIVERIES.load(Ordering::Relaxed),
        ipc_queue_bytes_current: IPC_QUEUE_CUR_BYTES.load(Ordering::Relaxed),
        ipc_queue_bytes_peak: IPC_QUEUE_PEAK_BYTES.load(Ordering::Relaxed),
        ipc_queue_messages_current: IPC_QUEUE_CUR_MESSAGES.load(Ordering::Relaxed),
        ipc_queue_messages_peak: IPC_QUEUE_PEAK_MESSAGES.load(Ordering::Relaxed),
        boot_token_grants: BOOT_TOKEN_GRANTS.load(Ordering::Relaxed),
        thread_suspend_calls: THREAD_SUSPEND_CALLS.load(Ordering::Relaxed),
        thread_suspend_success: THREAD_SUSPEND_SUCCESS.load(Ordering::Relaxed),
        thread_resume_calls: THREAD_RESUME_CALLS.load(Ordering::Relaxed),
        thread_resume_success: THREAD_RESUME_SUCCESS.load(Ordering::Relaxed),
        token_audit_next_seq: audit.next_seq,
        token_audit_stored: audit.stored,
        token_audit_dropped: audit.dropped,
        resources: resource_snapshot(),
    }
}

pub fn resource_snapshot() -> ResourceSnapshot {
    let (pmm_used, pmm_total) = crate::mm::pmm::get_stats();
    ResourceSnapshot {
        threads_total: crate::sched::ThreadManager::thread_count_total() as u64,
        threads_live: crate::sched::ThreadManager::thread_count_live() as u64,
        spaces: crate::mm::space_repository::count() as u64,
        endpoints: crate::ipc::endpoint::endpoint_count() as u64,
        tokens: crate::token::count_tokens() as u64,
        tracked_frames: crate::mm::frame_registry::tracked_count() as u64,
        mapped_frames: crate::mm::frame_registry::total_map_count(),
        pmm_used_frames: pmm_used as u64,
        pmm_total_frames: pmm_total as u64,
    }
}

pub fn set_resource_baseline_from_current() {
    *RESOURCE_BASELINE.lock() = Some(resource_snapshot());
}

fn resource_baseline() -> ResourceSnapshot {
    let mut guard = RESOURCE_BASELINE.lock();
    if let Some(baseline) = *guard {
        baseline
    } else {
        let baseline = resource_snapshot();
        *guard = Some(baseline);
        baseline
    }
}

pub fn log_resource_delta(reason: &str) {
    let seq = RESOURCE_DELTA_LOG_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let baseline = resource_baseline();
    let current = resource_snapshot();

    klibcluu::info("resource delta: ");
    klibcluu::info(reason);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  sample_seq=", seq);

    klibcluu::log_kv_i64(
        klibcluu::LogLevel::Info,
        "  delta_threads_live=",
        current.threads_live as i64 - baseline.threads_live as i64,
    );
    klibcluu::log_kv_i64(
        klibcluu::LogLevel::Info,
        "  delta_spaces=",
        current.spaces as i64 - baseline.spaces as i64,
    );
    klibcluu::log_kv_i64(
        klibcluu::LogLevel::Info,
        "  delta_endpoints=",
        current.endpoints as i64 - baseline.endpoints as i64,
    );
    klibcluu::log_kv_i64(
        klibcluu::LogLevel::Info,
        "  delta_tokens=",
        current.tokens as i64 - baseline.tokens as i64,
    );
    klibcluu::log_kv_i64(
        klibcluu::LogLevel::Info,
        "  delta_tracked_frames=",
        current.tracked_frames as i64 - baseline.tracked_frames as i64,
    );
    klibcluu::log_kv_i64(
        klibcluu::LogLevel::Info,
        "  delta_mapped_frames=",
        current.mapped_frames as i64 - baseline.mapped_frames as i64,
    );
    klibcluu::log_kv_i64(
        klibcluu::LogLevel::Info,
        "  delta_pmm_used_frames=",
        current.pmm_used_frames as i64 - baseline.pmm_used_frames as i64,
    );

    let s = snapshot();
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_wait_events=", s.ipc_recv_wait_events);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_wait_avg_ms=", s.ipc_recv_wait_avg_ms);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_wait_p95_ms=", s.ipc_recv_wait_p95_ms);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_wait_p99_ms=", s.ipc_recv_wait_p99_ms);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_wait_max_ms=", s.ipc_recv_wait_max_ms);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_scan_events=", s.ipc_recv_scan_events);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_scan_avg_steps_x100=", s.ipc_recv_scan_avg_steps_x100);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_scan_max_steps=", s.ipc_recv_scan_max_steps);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_stale_waiters=", s.ipc_stale_waiters);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_direct_deliveries=", s.ipc_direct_deliveries);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_queue_bytes_current=", s.ipc_queue_bytes_current);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_queue_bytes_peak=", s.ipc_queue_bytes_peak);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_queue_messages_current=", s.ipc_queue_messages_current);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_queue_messages_peak=", s.ipc_queue_messages_peak);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  thread_suspend_calls=", s.thread_suspend_calls);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  thread_suspend_success=", s.thread_suspend_success);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  thread_resume_calls=", s.thread_resume_calls);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  thread_resume_success=", s.thread_resume_success);
}

#[inline(always)]
fn object_fields(object_ref: ObjectRef) -> (u8, u64) {
    match object_ref {
        ObjectRef::Thread(id) => (1, id.as_u64()),
        ObjectRef::Space(id) => (2, id.as_u64()),
        ObjectRef::Endpoint(id) => (3, id.as_u64()),
        ObjectRef::Irq(irq) => (4, irq as u64),
        ObjectRef::Clock => (6, 0),
        ObjectRef::Frame(id) => (7, id.as_u64()),
        ObjectRef::Notification(id) => (8, id.as_u64()),
        ObjectRef::VfsViewManager { scope_sid, .. } => (9, scope_sid as u64),
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

    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  tokens_created=", s.tokens_created);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  tokens_revoked=", s.tokens_revoked);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_recv_would_block=", s.ipc_recv_would_block);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_recv_timeout=", s.ipc_recv_timeout);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_recv_wait_events=", s.ipc_recv_wait_events);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_recv_wait_avg_ms=", s.ipc_recv_wait_avg_ms);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_recv_wait_p95_ms=", s.ipc_recv_wait_p95_ms);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_recv_wait_p99_ms=", s.ipc_recv_wait_p99_ms);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_recv_wait_max_ms=", s.ipc_recv_wait_max_ms);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_recv_scan_events=", s.ipc_recv_scan_events);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_recv_scan_avg_steps_x100=", s.ipc_recv_scan_avg_steps_x100);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_recv_scan_max_steps=", s.ipc_recv_scan_max_steps);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_stale_waiters=", s.ipc_stale_waiters);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_direct_deliveries=", s.ipc_direct_deliveries);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_queue_bytes_current=", s.ipc_queue_bytes_current);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_queue_bytes_peak=", s.ipc_queue_bytes_peak);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_queue_messages_current=", s.ipc_queue_messages_current);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  ipc_queue_messages_peak=", s.ipc_queue_messages_peak);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  boot_token_grants=", s.boot_token_grants);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  thread_suspend_calls=", s.thread_suspend_calls);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  thread_suspend_success=", s.thread_suspend_success);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  thread_resume_calls=", s.thread_resume_calls);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  thread_resume_success=", s.thread_resume_success);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  token_audit_next_seq=", s.token_audit_next_seq);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  token_audit_stored=", s.token_audit_stored as u64);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  token_audit_dropped=", s.token_audit_dropped);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  resources_threads_total=", s.resources.threads_total);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  resources_threads_live=", s.resources.threads_live);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  resources_spaces=", s.resources.spaces);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  resources_endpoints=", s.resources.endpoints);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  resources_tokens=", s.resources.tokens);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  resources_tracked_frames=", s.resources.tracked_frames);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  resources_mapped_frames=", s.resources.mapped_frames);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  resources_pmm_used_frames=", s.resources.pmm_used_frames);
    klibcluu::log_kv_dec(klibcluu::LogLevel::Info, "  resources_pmm_total_frames=", s.resources.pmm_total_frames);

    if stage == "post-bootstrap" {
        set_resource_baseline_from_current();
    }
}

#[cfg(test)]
pub fn reset_for_tests() {
    TOKENS_CREATED.store(0, Ordering::Relaxed);
    TOKENS_REVOKED.store(0, Ordering::Relaxed);
    IPC_RECV_WOULD_BLOCK.store(0, Ordering::Relaxed);
    IPC_RECV_TIMEOUT.store(0, Ordering::Relaxed);
    IPC_RECV_WAIT_EVENTS.store(0, Ordering::Relaxed);
    IPC_RECV_WAIT_TOTAL_TICKS.store(0, Ordering::Relaxed);
    IPC_RECV_WAIT_MAX_TICKS.store(0, Ordering::Relaxed);
    IPC_RECV_SCAN_EVENTS.store(0, Ordering::Relaxed);
    IPC_RECV_SCAN_TOTAL_STEPS.store(0, Ordering::Relaxed);
    IPC_RECV_SCAN_MAX_STEPS.store(0, Ordering::Relaxed);
    IPC_STALE_WAITERS.store(0, Ordering::Relaxed);
    IPC_DIRECT_DELIVERIES.store(0, Ordering::Relaxed);
    IPC_QUEUE_CUR_BYTES.store(0, Ordering::Relaxed);
    IPC_QUEUE_PEAK_BYTES.store(0, Ordering::Relaxed);
    IPC_QUEUE_CUR_MESSAGES.store(0, Ordering::Relaxed);
    IPC_QUEUE_PEAK_MESSAGES.store(0, Ordering::Relaxed);
    BOOT_TOKEN_GRANTS.store(0, Ordering::Relaxed);
    THREAD_SUSPEND_CALLS.store(0, Ordering::Relaxed);
    THREAD_SUSPEND_SUCCESS.store(0, Ordering::Relaxed);
    THREAD_RESUME_CALLS.store(0, Ordering::Relaxed);
    THREAD_RESUME_SUCCESS.store(0, Ordering::Relaxed);
    RESOURCE_DELTA_LOG_SEQ.store(0, Ordering::Relaxed);
    for bucket in IPC_RECV_WAIT_HISTOGRAM.iter() {
        bucket.store(0, Ordering::Relaxed);
    }
    let mut audit = TOKEN_AUDIT_RING.lock();
    *audit = TokenAuditRing::new();
    *RESOURCE_BASELINE.lock() = None;
}
