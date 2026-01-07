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
}

impl TokenTableInner {
    const fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            scopes: BTreeMap::new(),
            next_handle: 1, // 0 is reserved as invalid handle
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

    /// Lookup a token by handle (mutable)
    fn get_mut(&mut self, handle: TokenHandle) -> Option<&mut Token> {
        self.handles.get_mut(&handle)
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
        self.handles.remove(&handle)
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
fn kernel_secret() -> [u8; 32] {
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

/// Lookup and validate a token
///
/// Returns the token if:
/// - Handle is valid
/// - Token hasn't expired
/// - Signature is valid
///
/// # Arguments
///
/// * `handle` - Token handle from userspace
///
/// # Returns
///
/// * `Ok(&Token)` - Valid token
/// * `Err(&str)` - Error reason
pub fn lookup_token(handle: TokenHandle) -> Result<Token, &'static str> {
    let table = TOKEN_TABLE.lock();

    // Lookup token
    let token = table.get(handle).ok_or("Invalid token handle")?;

    // Check expiration
    let now = current_timestamp();
    if token.is_expired(now) {
        return Err("Token expired");
    }

    // Verify signature
    let secret = kernel_secret();
    if !token.verify(&secret) {
        return Err("Invalid token signature");
    }

    Ok(token.clone())
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
        _ => Err("Object type mismatch"),
    }
}

/// Revoke a token
///
/// Removes token from table, making handle invalid.
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

    Ok(())
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
