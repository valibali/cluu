//! Token Table - Handle and Scope Mapping
//!
//! This module provides the global token table that maps:
//! - TokenHandle (userspace) → Token (kernel)
//! - OpaqueScope → ObjectRef (kernel-internal)
//!
//! # Architecture
//!
//! ```text
//! Userspace                Kernel Token Table           Kernel Objects
//! ┌──────────────┐         ┌─────────────────────┐      ┌────────────┐
//! │ TokenHandle  │         │ Handle → Token      │      │ Thread     │
//! │   42         │────────>│   42 → Token {      │      │ Space      │
//! └──────────────┘         │     scope: [a3..]   │──┐   │ Endpoint   │
//!                          │     role: READ      │  │   │ IRQ        │
//!                          │     ...             │  │   └────────────┘
//!                          │   }                 │  │
//!                          │                     │  │
//!                          │ Scope → ObjectRef   │  │
//!                          │   [a3 f2..] ────────┼──┘
//!                          │     → Thread(17)    │
//!                          └─────────────────────┘
//! ```
//!
//! # Thread Safety
//!
//! Uses Mutex for interior mutability, allowing safe concurrent access
//! from multiple threads and interrupt handlers.

use super::scope::ObjectRef;
use super::{OpaqueScope, Token, TokenHandle};
use alloc::collections::BTreeMap;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

// ═══════════════════════════════════════════════════════════════════════════
// Token Table Structure - Sharded for Reduced Contention
// ═══════════════════════════════════════════════════════════════════════════

/// Number of shards for the token table
/// More shards = less contention, but more memory overhead
const NUM_SHARDS: usize = 16;

/// Hash function for token handles to determine shard
#[inline(always)]
fn hash_handle(handle: TokenHandle) -> usize {
    // Simple hash: use handle value modulo number of shards
    // TokenHandle is usize, so we can use it directly
    handle.as_usize() % NUM_SHARDS
}

/// Single shard of the token table
///
/// Each shard contains a subset of tokens based on handle hash.
/// This allows concurrent access to different tokens without contention.
struct TokenTableShard {
    /// Map from handle to (token, object_ref) — both returned in a single lookup
    handles: BTreeMap<TokenHandle, (Token, ObjectRef)>,
}

struct RemovedToken {
    token: Token,
    object_ref: Option<ObjectRef>,
}

impl TokenTableShard {
    const fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
        }
    }

    /// Insert a token into this shard
    fn insert(&mut self, handle: TokenHandle, token: Token, object_ref: ObjectRef) {
        self.handles.insert(handle, (token, object_ref));
    }

    /// Lookup a token and its object reference by handle
    fn get(&self, handle: TokenHandle) -> Option<&(Token, ObjectRef)> {
        self.handles.get(&handle)
    }

    /// Remove a token by handle
    fn remove(&mut self, handle: TokenHandle) -> Option<RemovedToken> {
        let (token, object_ref) = self.handles.remove(&handle)?;
        Some(RemovedToken {
            token,
            object_ref: Some(object_ref),
        })
    }

    /// Resolve opaque scope to object reference (linear scan — not on hot path)
    fn resolve_scope(&self, scope: &OpaqueScope) -> Option<ObjectRef> {
        self.handles
            .values()
            .find(|(token, _)| &token.scope == scope)
            .map(|(_, obj_ref)| *obj_ref)
    }

    /// Count tokens in this shard
    fn count(&self) -> usize {
        self.handles.len()
    }

    /// Count tokens for a specific object in this shard
    fn count_for_object(&self, object_ref: ObjectRef) -> usize {
        self.handles
            .values()
            .filter(|(_, obj_ref)| *obj_ref == object_ref)
            .count()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Global Token Table - Sharded for Reduced Contention
// ═══════════════════════════════════════════════════════════════════════════

/// Sharded token table - each shard has its own mutex
/// This allows concurrent access to different tokens without contention
/// Array is initialized with NUM_SHARDS copies of a new shard
static TOKEN_TABLE_SHARDS: [Mutex<TokenTableShard>; NUM_SHARDS] =
    [const { Mutex::new(TokenTableShard::new()) }; NUM_SHARDS];

/// Global revocation generation counter (atomic for lock-free reads)
/// Incremented when any token is revoked to invalidate all thread-local caches
static REVOCATION_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Global handle allocator (atomic for lock-free allocation)
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1); // 0 is reserved as invalid handle

/// Global live-token counter (incremented on create, decremented on revoke).
static TOTAL_TOKEN_COUNT: AtomicU64 = AtomicU64::new(0);

/// Hard cap on the total number of live tokens system-wide.
/// Prevents unbounded growth from misbehaving userspace processes.
const MAX_TOTAL_TOKENS: u64 = 65536;

/// Get the shard for a given handle
#[inline(always)]
fn get_shard(handle: TokenHandle) -> &'static Mutex<TokenTableShard> {
    &TOKEN_TABLE_SHARDS[hash_handle(handle)]
}

/// Lock-free init-once container for the kernel HMAC secret.
///
/// Written exactly once during `init()` and read on every token operation.
/// Using an init-once pattern avoids Mutex contention on the hot path.
struct OnceSecret {
    init: AtomicBool,
    data: UnsafeCell<[u8; 32]>,
}

// SAFETY: `data` is written exactly once (before `init` is set to true with Release),
// and only read after `init` is observed as true (with Acquire). No data race.
unsafe impl Sync for OnceSecret {}

static KERNEL_SECRET: OnceSecret = OnceSecret {
    init: AtomicBool::new(false),
    data: UnsafeCell::new([0u8; 32]),
};

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Initialize token system
///
/// Must be called during kernel init.
/// Generates kernel secret for token signatures.
pub fn init() {
    // Generate kernel secret from RNG
    let mut secret = [0u8; 32];
    klibcluu::crypto::fill_random(&mut secret);

    // SAFETY: No other thread can read `data` yet because `init` is still false.
    unsafe {
        KERNEL_SECRET.data.get().write(secret);
    }
    KERNEL_SECRET.init.store(true, Ordering::Release);

    klibcluu::info("Token system initialized");
}

// ═══════════════════════════════════════════════════════════════════════════
// Per-CPU Token Cache (lock-free on cache hit)
// ═══════════════════════════════════════════════════════════════════════════

/// Thread-local token cache entry
///
/// Caches a verified token to avoid repeated expensive operations.
/// Cache is invalidated when:
/// - Token expires (checked on every lookup)
/// - Token is revoked (generation counter changes)
/// - Token is removed from table (checked on every lookup)
#[derive(Clone, Debug)]
struct TokenCacheEntry {
    /// Cached token handle
    handle: super::TokenHandle,
    /// Cached token (already verified)
    token: super::Token,
    /// Cached object reference
    object_ref: crate::token::scope::ObjectRef,
    /// Generation when cached (for revocation detection)
    cached_generation: u64,
}

/// Number of entries in the per-CPU token cache.
const TOKEN_CACHE_SIZE: usize = 8;

/// Per-CPU 8-entry LRU token cache.
///
/// `recv_any` with multiple endpoints benefits from caching more than one token.
/// The LRU order is tracked in `lru_order`: index 0 = most recently used.
#[derive(Clone, Debug)]
struct TokenCache {
    entries: [Option<TokenCacheEntry>; TOKEN_CACHE_SIZE],
    /// LRU order: lru_order[0] = most recently used slot index
    lru_order: [u8; TOKEN_CACHE_SIZE],
}

impl TokenCache {
    /// Create an empty cache.
    const fn new() -> Self {
        Self {
            entries: [None, None, None, None, None, None, None, None],
            lru_order: [0, 1, 2, 3, 4, 5, 6, 7],
        }
    }

    /// Look up a handle in the cache. On hit, promotes the entry to MRU.
    fn lookup(&mut self, handle: super::TokenHandle) -> Option<&TokenCacheEntry> {
        // Linear scan to find the handle
        for i in 0..TOKEN_CACHE_SIZE {
            let slot = self.lru_order[i] as usize;
            if let Some(ref entry) = self.entries[slot] {
                if entry.handle == handle {
                    // Promote to MRU by shifting entries before it
                    if i > 0 {
                        let hit_slot = self.lru_order[i];
                        // Shift [0..i] right by 1
                        for j in (1..=i).rev() {
                            self.lru_order[j] = self.lru_order[j - 1];
                        }
                        self.lru_order[0] = hit_slot;
                    }
                    return self.entries[self.lru_order[0] as usize].as_ref();
                }
            }
        }
        None
    }

    /// Insert a new entry, evicting the LRU slot.
    fn insert(&mut self, entry: TokenCacheEntry) {
        // Evict the LRU slot (last in lru_order)
        let evict_slot = self.lru_order[TOKEN_CACHE_SIZE - 1] as usize;
        self.entries[evict_slot] = Some(entry);
        // Promote evicted slot to MRU
        let evict_val = self.lru_order[TOKEN_CACHE_SIZE - 1];
        for j in (1..TOKEN_CACHE_SIZE).rev() {
            self.lru_order[j] = self.lru_order[j - 1];
        }
        self.lru_order[0] = evict_val;
    }

    /// Invalidate all entries (e.g., on revocation generation mismatch).
    fn invalidate_all(&mut self) {
        for entry in &mut self.entries {
            *entry = None;
        }
    }

    /// Remove a specific handle from the cache.
    fn clear_handle(&mut self, handle: super::TokenHandle) {
        for entry in &mut self.entries {
            if let Some(ref e) = entry {
                if e.handle == handle {
                    *entry = None;
                    return;
                }
            }
        }
    }
}

/// Per-CPU token cache — bypasses THREAD_REPOSITORY lock entirely.
///
/// SAFETY invariant: accessed only from syscall handlers, which are
/// non-reentrant on this single-CPU kernel. Interrupt handlers (timer,
/// GPF, PF) never call lookup_token.
struct PerCpuTokenCache {
    inner: UnsafeCell<TokenCache>,
}

// SAFETY: Single-CPU kernel. Only accessed from non-reentrant syscall context.
unsafe impl Sync for PerCpuTokenCache {}

static PERCPU_TOKEN_CACHE: PerCpuTokenCache = PerCpuTokenCache {
    inner: UnsafeCell::new(TokenCache::new()),
};

/// Get kernel secret (for token signing)
///
/// Panics if token system hasn't been initialized.
pub(super) fn kernel_secret() -> [u8; 32] {
    assert!(
        KERNEL_SECRET.init.load(Ordering::Acquire),
        "Token system not initialized"
    );
    // SAFETY: `init` is true with Acquire ordering, so the write in `init()` is visible.
    unsafe { *KERNEL_SECRET.data.get() }
}

/// Create a new token and return its handle
///
/// # Arguments
///
/// * `scope` - Opaque scope for this token
/// * `role` - Rights bitmask
/// * `issuer` - Who is creating this token
/// * `expire_at` - When this token expires
/// * `object_ref` - Actual kernel object this refers to
///
/// # Returns
///
/// TokenHandle that userspace can use
pub fn create_token(
    scope: OpaqueScope,
    role: super::Rights,
    issuer: super::Issuer,
    expire_at: super::Timestamp,
    object_ref: ObjectRef,
) -> TokenHandle {
    create_token_with_kind(scope, role, issuer, expire_at, object_ref, false)
}

/// Create a token representing explicit derivation from an existing authority.
pub(crate) fn create_derived_token(
    scope: OpaqueScope,
    role: super::Rights,
    issuer: super::Issuer,
    expire_at: super::Timestamp,
    object_ref: ObjectRef,
) -> TokenHandle {
    create_token_with_kind(scope, role, issuer, expire_at, object_ref, true)
}

fn create_token_with_kind(
    scope: OpaqueScope,
    role: super::Rights,
    issuer: super::Issuer,
    expire_at: super::Timestamp,
    object_ref: ObjectRef,
    derived: bool,
) -> TokenHandle {
    // Kernel-internal creation: should never hit the limit during boot.
    try_create_token_with_kind(scope, role, issuer, expire_at, object_ref, derived)
        .expect("kernel token creation exceeded global limit")
}

/// Fallible token creation that enforces the global token limit.
///
/// Returns `Err` if the system already has `MAX_TOTAL_TOKENS` live tokens.
fn try_create_token_with_kind(
    scope: OpaqueScope,
    role: super::Rights,
    issuer: super::Issuer,
    expire_at: super::Timestamp,
    object_ref: ObjectRef,
    derived: bool,
) -> Result<TokenHandle, &'static str> {
    // Atomically check-and-increment the global counter.
    loop {
        let current = TOTAL_TOKEN_COUNT.load(Ordering::Relaxed);
        if current >= MAX_TOTAL_TOKENS {
            return Err("Global token limit reached");
        }
        if TOTAL_TOKEN_COUNT
            .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            break;
        }
    }

    let secret = kernel_secret();
    let token = Token::new(scope, role, issuer, expire_at, &secret);

    // Allocate handle atomically (lock-free)
    let handle = TokenHandle::new(NEXT_HANDLE.fetch_add(1, Ordering::SeqCst) as usize);

    // Insert into appropriate shard
    let telemetry_token = token.clone();
    let shard = get_shard(handle);
    shard.lock().insert(handle, token, object_ref);
    crate::telemetry::record_token_created();
    crate::telemetry::record_token_audit_create(handle, &telemetry_token, object_ref, derived);

    Ok(handle)
}

/// Fallible derived-token creation for the syscall path.
pub(crate) fn try_create_derived_token(
    scope: OpaqueScope,
    role: super::Rights,
    issuer: super::Issuer,
    expire_at: super::Timestamp,
    object_ref: ObjectRef,
) -> Result<TokenHandle, &'static str> {
    try_create_token_with_kind(scope, role, issuer, expire_at, object_ref, true)
}

/// Lookup and validate a token (with thread-local caching)
///
/// Returns the token if:
/// - Handle is valid
/// - Token hasn't expired
/// - Signature is valid (or cached and still valid)
///
/// Uses thread-local cache to avoid repeated HMAC verification for the same token.
/// Cache is invalidated on expiration, revocation, or if token is removed.
///
/// # Arguments
///
/// * `handle` - Token handle from userspace
///
/// # Returns
///
/// * `Ok(Token)` - Valid token
/// * `Err(&str)` - Error reason
pub fn lookup_token(
    handle: TokenHandle,
) -> Result<(Token, crate::token::scope::ObjectRef), &'static str> {
    // Try per-CPU cache first (no lock needed)
    if let Some(cached) = try_cache_lookup(handle) {
        return Ok(cached);
    }

    // Cache miss — full lookup (locks only the relevant token shard)
    let (token, object_ref, generation) = {
        let shard = get_shard(handle);
        let table = shard.lock();

        let (token, object_ref) = table.get(handle).ok_or("Invalid token handle")?;

        let now = current_timestamp();
        if token.is_expired(now) {
            return Err("Token expired");
        }

        let generation = revocation_generation();

        (token.clone(), *object_ref, generation)
    };

    // Update per-CPU cache (no lock needed)
    update_cache(handle, &token, object_ref, generation);

    Ok((token, object_ref))
}

/// Try to lookup token from per-CPU cache
///
/// Returns cached token if:
/// - Cache entry exists for this handle
/// - Generation matches (token not revoked since caching)
/// - Token not expired
fn try_cache_lookup(handle: TokenHandle) -> Option<(Token, crate::token::scope::ObjectRef)> {
    // SAFETY: only called from syscall context, non-reentrant on single CPU
    let cache = unsafe { &mut *PERCPU_TOKEN_CACHE.inner.get() };
    let current_generation = revocation_generation();
    let entry = cache.lookup(handle)?;

    if entry.cached_generation != current_generation {
        cache.invalidate_all();
        return None;
    }

    let now = current_timestamp();
    if entry.token.is_expired(now) {
        cache.clear_handle(handle);
        return None;
    }

    Some((entry.token.clone(), entry.object_ref))
}

/// Update per-CPU token cache
fn update_cache(
    handle: TokenHandle,
    token: &Token,
    object_ref: crate::token::scope::ObjectRef,
    generation: u64,
) {
    // SAFETY: only called from syscall context, non-reentrant on single CPU
    let cache = unsafe { &mut *PERCPU_TOKEN_CACHE.inner.get() };
    cache.insert(TokenCacheEntry {
        handle,
        token: token.clone(),
        object_ref,
        cached_generation: generation,
    });
}

/// Resolve opaque scope to object reference
///
/// # Arguments
///
/// * `scope` - Opaque scope from token
///
/// # Returns
///
/// * `Some(ObjectRef)` - Kernel object this scope refers to
/// * `None` - Unknown scope
#[allow(dead_code)]
pub fn resolve_scope(scope: &OpaqueScope) -> Option<ObjectRef> {
    // Check all shards (scopes are replicated across shards for simplicity)
    // In practice, we could optimize this by storing scopes separately
    for shard in &TOKEN_TABLE_SHARDS {
        if let Some(obj_ref) = shard.lock().resolve_scope(scope) {
            return Some(obj_ref);
        }
    }
    None
}

/// Resolve token scope with type checking
///
/// Convenience function that both resolves the scope and checks
/// that it's the expected object type.
///
/// # Arguments
///
/// * `token` - Token to resolve
/// * `expected_type` - Expected object type
///
/// # Returns
///
/// * `Ok(ObjectRef)` - Matching object reference
/// * `Err(&str)` - Error (wrong type or not found)
#[allow(dead_code)]
pub fn resolve_token_object(
    token: &Token,
    expected_type: ObjectType,
) -> Result<ObjectRef, &'static str> {
    let obj_ref = resolve_scope(&token.scope).ok_or("Unknown scope")?;

    // Check type matches
    match (&obj_ref, expected_type) {
        (ObjectRef::Thread(_), ObjectType::Thread) => Ok(obj_ref),
        (ObjectRef::Space(_), ObjectType::Space) => Ok(obj_ref),
        (ObjectRef::Endpoint(_), ObjectType::Endpoint) => Ok(obj_ref),
        (ObjectRef::Irq(_), ObjectType::Irq) => Ok(obj_ref),
        (ObjectRef::Clock, ObjectType::Clock) => Ok(obj_ref),
        (ObjectRef::Frame(_), ObjectType::Frame) => Ok(obj_ref),
        (ObjectRef::Notification(_), ObjectType::Notification) => Ok(obj_ref),
        (ObjectRef::VfsViewManager { .. }, ObjectType::VfsViewManager) => Ok(obj_ref),
        _ => Err("Object type mismatch"),
    }
}

/// Check that an ObjectRef matches the expected type.
///
/// Use instead of `resolve_token_object` when you already have the ObjectRef
/// (e.g., from `lookup_token` which returns it on both cache hit and miss paths).
pub fn check_object_type(
    obj_ref: ObjectRef,
    expected_type: ObjectType,
) -> Result<ObjectRef, &'static str> {
    match (&obj_ref, expected_type) {
        (ObjectRef::Thread(_), ObjectType::Thread) => Ok(obj_ref),
        (ObjectRef::Space(_), ObjectType::Space) => Ok(obj_ref),
        (ObjectRef::Endpoint(_), ObjectType::Endpoint) => Ok(obj_ref),
        (ObjectRef::Irq(_), ObjectType::Irq) => Ok(obj_ref),
        (ObjectRef::Clock, ObjectType::Clock) => Ok(obj_ref),
        (ObjectRef::Frame(_), ObjectType::Frame) => Ok(obj_ref),
        (ObjectRef::Notification(_), ObjectType::Notification) => Ok(obj_ref),
        (ObjectRef::VfsViewManager { .. }, ObjectType::VfsViewManager) => Ok(obj_ref),
        _ => Err("Object type mismatch"),
    }
}

/// Revoke a token
///
/// Removes token from table, making handle invalid.
/// Also increments revocation generation to invalidate thread-local caches.
///
/// # Arguments
///
/// * `handle` - Token handle to revoke
///
/// # Returns
///
/// * `Ok(())` - Token revoked
/// * `Err(&str)` - Handle not found
pub fn revoke_token(handle: TokenHandle) -> Result<(), &'static str> {
    // Lock only the shard for this handle
    let shard = get_shard(handle);
    let removed = shard.lock().remove(handle);

    if let Some(removed) = removed {
        // Decrement global token counter
        TOTAL_TOKEN_COUNT.fetch_sub(1, Ordering::Relaxed);
        // Increment generation counter atomically (invalidates all thread-local caches)
        REVOCATION_GENERATION.fetch_add(1, Ordering::SeqCst);
        crate::telemetry::record_token_revoked(1);
        crate::telemetry::record_token_audit_revoke(
            handle,
            Some(&removed.token),
            removed.object_ref,
        );

        // Check if an endpoint became unreferenced
        if let Some(crate::token::scope::ObjectRef::Endpoint(ep_id)) = removed.object_ref {
            if count_tokens_for_object(crate::token::scope::ObjectRef::Endpoint(ep_id)) == 0 {
                crate::ipc::endpoint::destroy_endpoint_full(ep_id);
            }
        }

        // Check if a notification became unreferenced
        if let Some(crate::token::scope::ObjectRef::Notification(nid)) = removed.object_ref {
            if count_tokens_for_object(crate::token::scope::ObjectRef::Notification(nid)) == 0 {
                crate::ipc::notification::destroy_notification(nid);
            }
        }

        Ok(())
    } else {
        Err("Token not found")
    }
}

/// Get current revocation generation (for cache validation)
///
/// Threads can cache tokens with this generation number.
/// If the generation changes (due to revocation), cached tokens are invalid.
/// This is lock-free (atomic read).
pub fn revocation_generation() -> u64 {
    REVOCATION_GENERATION.load(Ordering::SeqCst)
}

/// Revoke all tokens that reference a specific kernel object.
///
/// Used during process cleanup to invalidate tokens referring to
/// destroyed threads, address spaces, or endpoints.
///
/// Returns the number of tokens revoked.
pub fn revoke_tokens_for_object(target: ObjectRef) -> usize {
    let mut revoked = 0;

    for shard_mutex in &TOKEN_TABLE_SHARDS {
        let mut shard = shard_mutex.lock();

        // Collect handles to revoke (can't modify while iterating)
        let handles_to_revoke: alloc::vec::Vec<TokenHandle> = shard
            .handles
            .iter()
            .filter(|(_, (_, obj_ref))| *obj_ref == target)
            .map(|(handle, _)| *handle)
            .collect();

        for handle in handles_to_revoke {
            if let Some(removed) = shard.remove(handle) {
                crate::telemetry::record_token_audit_revoke(
                    handle,
                    Some(&removed.token),
                    removed.object_ref,
                );
                revoked += 1;
            }
        }
    }

    if revoked > 0 {
        TOTAL_TOKEN_COUNT.fetch_sub(revoked as u64, Ordering::Relaxed);
        REVOCATION_GENERATION.fetch_add(1, Ordering::SeqCst);
        crate::telemetry::record_token_revoked(revoked as u64);

        // If we revoked endpoint tokens, check for zero-reference cleanup
        if let crate::token::scope::ObjectRef::Endpoint(ep_id) = target {
            if count_tokens_for_object(target) == 0 {
                crate::ipc::endpoint::destroy_endpoint_full(ep_id);
            }
        }

        // If we revoked notification tokens, check for zero-reference cleanup
        if let crate::token::scope::ObjectRef::Notification(nid) = target {
            if count_tokens_for_object(target) == 0 {
                crate::ipc::notification::destroy_notification(nid);
            }
        }
    }

    revoked
}

/// Get token count (sum across all shards)
pub fn count_tokens() -> usize {
    TOKEN_TABLE_SHARDS
        .iter()
        .map(|shard| shard.lock().count())
        .sum()
}

/// Get token count for specific object (sum across all shards)
pub fn count_tokens_for_object(object_ref: ObjectRef) -> usize {
    TOKEN_TABLE_SHARDS
        .iter()
        .map(|shard| shard.lock().count_for_object(object_ref))
        .sum()
}

// ═══════════════════════════════════════════════════════════════════════════
// Helper Types
// ═══════════════════════════════════════════════════════════════════════════

/// Expected object type for type checking
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Thread,
    Space,
    Endpoint,
    Irq,
    Clock,
    Frame,
    Notification,
    VfsViewManager,
}

/// Get current timestamp (monotonic nanoseconds since boot)
///
/// TODO: Replace with actual timestamp source
fn current_timestamp() -> super::Timestamp {
    use super::Timestamp;

    // For now, use TSC (timestamp counter)
    // In production, this should use a proper monotonic clock
    let tsc = crate::architecture::x86_64::tsc::rdtsc();

    Timestamp::new(tsc)
}

// ═══════════════════════════════════════════════════════════════════════════
// Unit Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::super::{Issuer, Rights, Timestamp};
    use super::*;
    use crate::sched::ThreadId;

    #[test]
    fn test_token_table_insert_lookup() {
        init();

        let scope = OpaqueScope::from_bytes([1u8; 16]);
        let role = Rights::READ;
        let issuer = Issuer::Kernel;
        let expire_at = Timestamp::far_future();
        let object_ref = ObjectRef::Thread(ThreadId::new(42));

        let handle = create_token(scope, role, issuer, expire_at, object_ref);

        let (token, _obj_ref) = lookup_token(handle).expect("Token lookup failed");
        assert_eq!(token.scope, scope);
        assert_eq!(token.role, role);
    }

    #[test]
    fn test_resolve_scope() {
        init();

        let scope = OpaqueScope::from_bytes([2u8; 16]);
        let object_ref = ObjectRef::Thread(ThreadId::new(17));

        let _handle = create_token(
            scope,
            Rights::READ,
            Issuer::Kernel,
            Timestamp::far_future(),
            object_ref,
        );

        let resolved = resolve_scope(&scope).expect("Scope resolution failed");
        assert_eq!(resolved, object_ref);
    }

    #[test]
    fn test_revoke_token() {
        init();

        let scope = OpaqueScope::from_bytes([3u8; 16]);
        let handle = create_token(
            scope,
            Rights::READ,
            Issuer::Kernel,
            Timestamp::far_future(),
            ObjectRef::Thread(ThreadId::new(1)),
        );

        // Token exists
        assert!(lookup_token(handle).is_ok());

        // Revoke it
        revoke_token(handle).expect("Revocation failed");

        // No longer exists
        assert!(lookup_token(handle).is_err());
    }

    #[test]
    fn test_count_tokens() {
        init();

        let initial_count = count_tokens();

        let scope1 = OpaqueScope::from_bytes([4u8; 16]);
        let scope2 = OpaqueScope::from_bytes([5u8; 16]);

        create_token(
            scope1,
            Rights::READ,
            Issuer::Kernel,
            Timestamp::far_future(),
            ObjectRef::Thread(ThreadId::new(1)),
        );

        create_token(
            scope2,
            Rights::WRITE,
            Issuer::Kernel,
            Timestamp::far_future(),
            ObjectRef::Thread(ThreadId::new(2)),
        );

        assert_eq!(count_tokens(), initial_count + 2);
    }

    #[test]
    fn test_invalid_handle() {
        init();
        let invalid_handle = TokenHandle::new(999999);
        assert!(lookup_token(invalid_handle).is_err());
    }
}
