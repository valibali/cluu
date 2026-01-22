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
use spin::Mutex;

// ═══════════════════════════════════════════════════════════════════════════
// Token Table Structure
// ═══════════════════════════════════════════════════════════════════════════

/// Global token table
///
/// Maintains bidirectional mappings:
/// - TokenHandle → Token (for syscall validation)
/// - OpaqueScope → ObjectRef (for resolving objects)
struct TokenTableInner {
    /// Map from handle to token
    handles: BTreeMap<TokenHandle, Token>,

    /// Map from opaque scope to object reference
    scopes: BTreeMap<OpaqueScope, ObjectRef>,

    /// Next handle to allocate
    next_handle: usize,

    /// Generation counter - increments on revocation to invalidate caches
    /// Threads cache tokens with this generation; if it changes, cache is stale
    revocation_generation: u64,
}

impl TokenTableInner {
    const fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            scopes: BTreeMap::new(),
            next_handle: 1, // 0 is reserved as invalid handle
            revocation_generation: 0,
        }
    }

    /// Allocate a new token handle
    fn alloc_handle(&mut self) -> TokenHandle {
        let handle = TokenHandle::new(self.next_handle);
        self.next_handle += 1;
        handle
    }

    /// Insert a token and return its handle
    ///
    /// Also registers the scope → object mapping if not already present.
    fn insert(&mut self, token: Token, object_ref: ObjectRef) -> TokenHandle {
        let handle = self.alloc_handle();

        // Register scope → object mapping
        self.scopes.insert(token.scope, object_ref);

        // Register handle → token mapping
        self.handles.insert(handle, token);

        handle
    }

    /// Lookup a token by handle
    fn get(&self, handle: TokenHandle) -> Option<&Token> {
        self.handles.get(&handle)
    }

    /// Resolve opaque scope to object reference
    fn resolve_scope(&self, scope: &OpaqueScope) -> Option<ObjectRef> {
        self.scopes.get(scope).copied()
    }

    /// Remove a token by handle
    ///
    /// Note: This doesn't remove the scope mapping, as the same scope
    /// might be used in other tokens.
    fn remove(&mut self, handle: TokenHandle) -> Option<Token> {
        let removed = self.handles.remove(&handle);
        if removed.is_some() {
            // Increment generation to invalidate all thread-local caches
            self.revocation_generation = self.revocation_generation.wrapping_add(1);
        }
        removed
    }

    /// Get current revocation generation (for cache validation)
    fn revocation_generation(&self) -> u64 {
        self.revocation_generation
    }

    /// Count total tokens
    fn count(&self) -> usize {
        self.handles.len()
    }

    /// Count tokens for a specific object
    fn count_for_object(&self, object_ref: ObjectRef) -> usize {
        self.handles
            .values()
            .filter(|token| {
                self.scopes
                    .get(&token.scope)
                    .map(|obj| *obj == object_ref)
                    .unwrap_or(false)
            })
            .count()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Global Token Table
// ═══════════════════════════════════════════════════════════════════════════

/// Global token table instance
static TOKEN_TABLE: Mutex<TokenTableInner> = Mutex::new(TokenTableInner::new());

/// Kernel secret for HMAC signatures
///
/// In production, this should be:
/// - Generated at boot from hardware RNG
/// - Stored in protected memory (write-once)
/// - Never exposed to userspace
///
/// For now, we use a static key (should be replaced with proper initialization).
static KERNEL_SECRET: Mutex<Option<[u8; 32]>> = Mutex::new(None);

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

    *KERNEL_SECRET.lock() = Some(secret);

    klibcluu::info("Token system initialized");
}

/// Get kernel secret (for token signing)
///
/// Panics if token system hasn't been initialized.
pub(super) fn kernel_secret() -> [u8; 32] {
    KERNEL_SECRET.lock().expect("Token system not initialized")
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
    let secret = kernel_secret();
    let token = Token::new(scope, role, issuer, expire_at, &secret);

    TOKEN_TABLE.lock().insert(token, object_ref)
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
pub fn lookup_token(handle: TokenHandle) -> Result<Token, &'static str> {
    // Try to use thread-local cache if available
    if let Some(current_thread_id) = crate::sched::ThreadManager::current() {
        if let Some(cached) = try_cache_lookup(handle, current_thread_id) {
            return Ok(cached);
        }
    }

    // Cache miss or no current thread - do full lookup
    let (token, object_ref, generation) = {
        let table = TOKEN_TABLE.lock();

        // Lookup token
        let token = table.get(handle).ok_or("Invalid token handle")?;

        // Check expiration (always check - can't cache expiration)
        let now = current_timestamp();
        if token.is_expired(now) {
            return Err("Token expired");
        }

        // Verify signature (expensive operation - this is what we're caching)
        let secret = kernel_secret();
        if !token.verify(&secret) {
            return Err("Invalid token signature");
        }

        // Get object ref and generation for caching
        let object_ref = table.resolve_scope(&token.scope).ok_or("Unknown scope")?;
        let generation = table.revocation_generation();

        (token.clone(), object_ref, generation)
    };

    // Update thread-local cache
    if let Some(current_thread_id) = crate::sched::ThreadManager::current() {
        update_cache(current_thread_id, handle, &token, object_ref, generation);
    }

    Ok(token)
}

/// Try to lookup token from thread-local cache
///
/// Returns cached token if:
/// - Cache entry exists for this handle
/// - Generation matches (token not revoked)
/// - Token not expired
/// - Token still exists in table
fn try_cache_lookup(
    handle: TokenHandle,
    thread_id: crate::sched::thread::ThreadId,
) -> Option<Token> {
    use crate::sched::thread_manager::ThreadManager;

    // Get cached entry (if any)
    let cache = ThreadManager::with_thread(thread_id, |thread| thread.token_cache.clone())??; // Unwrap Option<Option<TokenCacheEntry>>

    // Check if cache entry matches this handle
    if cache.handle != handle {
        return None; // Cache miss
    }

    // Check generation (detects revocation)
    let current_generation = revocation_generation();
    if cache.cached_generation != current_generation {
        // Cache invalid - token was revoked, clear it
        ThreadManager::with_thread_mut(thread_id, |thread| {
            thread.token_cache = None;
        });
        return None;
    }

    // Check expiration (always check - can't cache expiration check)
    let now = current_timestamp();
    if cache.token.is_expired(now) {
        // Cache invalid - token expired, clear it
        ThreadManager::with_thread_mut(thread_id, |thread| {
            thread.token_cache = None;
        });
        return None;
    }

    // Verify token still exists in table (defense in depth)
    {
        let table = TOKEN_TABLE.lock();
        if table.get(handle).is_none() {
            // Token was removed - invalidate cache
            ThreadManager::with_thread_mut(thread_id, |thread| {
                thread.token_cache = None;
            });
            return None;
        }
    }

    // Cache hit! Return cached token (skip HMAC verification)
    Some(cache.token.clone())
}

/// Update thread-local token cache
fn update_cache(
    thread_id: crate::sched::thread::ThreadId,
    handle: TokenHandle,
    token: &Token,
    object_ref: crate::token::scope::ObjectRef,
    generation: u64,
) {
    use crate::sched::thread::TokenCacheEntry;
    use crate::sched::thread_manager::ThreadManager;

    ThreadManager::with_thread_mut(thread_id, |thread| {
        thread.token_cache = Some(TokenCacheEntry {
            handle,
            token: token.clone(),
            object_ref,
            cached_generation: generation,
        });
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
pub fn resolve_scope(scope: &OpaqueScope) -> Option<ObjectRef> {
    TOKEN_TABLE.lock().resolve_scope(scope)
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
        (ObjectRef::Reply(_), ObjectType::Reply) => Ok(obj_ref),
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
    TOKEN_TABLE.lock().remove(handle).ok_or("Token not found")?;
    // Generation counter is incremented by remove() to invalidate caches
    Ok(())
}

/// Get current revocation generation (for cache validation)
///
/// Threads can cache tokens with this generation number.
/// If the generation changes (due to revocation), cached tokens are invalid.
pub fn revocation_generation() -> u64 {
    TOKEN_TABLE.lock().revocation_generation()
}

/// Get token count
pub fn count_tokens() -> usize {
    TOKEN_TABLE.lock().count()
}

/// Get token count for specific object
pub fn count_tokens_for_object(object_ref: ObjectRef) -> usize {
    TOKEN_TABLE.lock().count_for_object(object_ref)
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
    Reply,
}

/// Get current timestamp (monotonic nanoseconds since boot)
///
/// TODO: Replace with actual timestamp source
fn current_timestamp() -> super::Timestamp {
    use super::Timestamp;

    // For now, use TSC (timestamp counter)
    // In production, this should use a proper monotonic clock
    let tsc = unsafe {
        let mut tsc: u64;
        core::arch::asm!("rdtsc", out("rax") tsc, out("rdx") _, options(nomem, nostack));
        tsc
    };

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

        let token = lookup_token(handle).expect("Token lookup failed");
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
