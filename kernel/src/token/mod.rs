//! Token-based Authorization System
//!
//! This module implements a JWT-like token system for capability-based security.
//!
//! # Token Model
//!
//! A token consists of:
//! - **scope**: Opaque, non-enumerable reference to a kernel object
//! - **role**: Rights bitmask (what operations are allowed)
//! - **issuer**: Who created this token (kernel or userspace authority)
//! - **expire_at**: Mandatory expiration timestamp
//! - **signature**: HMAC binding all fields (prevents forgery)
//!
//! # Design Goals
//!
//! 1. **Unforgeable**: Only kernel can create valid tokens (has secret key)
//! 2. **Non-enumerable**: Opaque scopes prevent object discovery
//! 3. **Time-bounded**: Mandatory expiration limits token lifetime
//! 4. **Least Privilege**: Rights masks enforce minimum permissions
//! 5. **Explicit Delegation**: Issuer field tracks delegation chains
//! 6. **Tamper-proof**: Signature prevents modification
//!
//! # Example
//!
//! ```rust,no_run
//! // Kernel mints root token for init process
//! let root_token = Token::new(
//!     OpaqueScope::random(),
//!     Rights::all(),
//!     Issuer::Kernel,
//!     Timestamp::far_future(),
//!     &kernel_secret,
//! );
//!
//! // Userspace derives restricted token
//! let limited_token = root_token.derive(
//!     Rights::READ,  // Reduced rights
//!     Timestamp::now() + Duration::minutes(5),  // Short expiration
//!     Issuer::Authority(vfs_server_id),
//!     &kernel_secret,
//! );
//! ```

pub mod rights;
pub mod scope;
pub mod signature;
pub mod table;

pub use rights::Rights;
pub use scope::{AddressSpaceId, EndpointId, ObjectRef, OpaqueScope, ReplyId};
pub use signature::Signature;
pub use table::{
    count_tokens, count_tokens_for_object, create_token, init as init_token_table, lookup_token,
    resolve_scope, resolve_token_object, revocation_generation, revoke_token, ObjectType,
};

/// Derive a new token from an existing one.
pub fn derive_token(
    parent: &Token,
    new_rights: Rights,
    new_expire: Timestamp,
    issuer: Issuer,
    object_ref: scope::ObjectRef,
) -> Option<TokenHandle> {
    let secret = table::kernel_secret();
    let derived = parent.derive(new_rights, new_expire, issuer, &secret)?;

    Some(table::create_token(
        derived.scope,
        derived.role,
        derived.issuer,
        derived.expire_at,
        object_ref,
    ))
}

/// Initialize the token subsystem.
pub fn init() {
    table::init();
}

// ═══════════════════════════════════════════════════════════════════════════
// Core Types
// ═══════════════════════════════════════════════════════════════════════════

/// Token handle (userspace-visible identifier)
///
/// This is what userspace passes to syscalls. The kernel maintains
/// a mapping from TokenHandle → Token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct TokenHandle(pub usize);

impl TokenHandle {
    pub const fn new(handle: usize) -> Self {
        Self(handle)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub const fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    pub const fn as_raw(self) -> usize {
        self.0
    }
}

/// Monotonic timestamp (nanoseconds since boot)
///
/// Used for token expiration. Monotonic ensures time always advances,
/// even if system clock is adjusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub const fn new(nanos: u64) -> Self {
        Self(nanos)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Timestamp far in the future (for root tokens that don't expire)
    pub const fn far_future() -> Self {
        Self(u64::MAX)
    }

    /// Never expires
    pub const NEVER: Self = Self(u64::MAX);
}

/// Token issuer - identifies who created the token
///
/// Enables tracking delegation chains:
/// - Kernel mints root authority
/// - Userspace services mint derived authority
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Issuer {
    /// Kernel-minted root authority
    ///
    /// Created during boot or by kernel operations.
    /// Represents fundamental authority.
    Kernel,

    /// Userspace authority (server/service ID)
    ///
    /// Userspace services can mint derived tokens for resources
    /// they manage. Example: VFS mints file descriptor tokens.
    Authority(AuthorityId),
}

impl Issuer {
    /// Serialize issuer to bytes for signature computation
    pub fn to_bytes(&self) -> [u8; 8] {
        match self {
            Issuer::Kernel => [0u8; 8],
            Issuer::Authority(id) => id.0.to_le_bytes(),
        }
    }
}

/// Authority identifier (userspace service)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityId(pub u64);

impl AuthorityId {
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Token Structure
// ═══════════════════════════════════════════════════════════════════════════

/// Token = unforgeable authority to perform operations on an object
///
/// Analogous to JWT with:
/// - scope = sub (subject)
/// - role = permissions
/// - issuer = iss (issuer)
/// - expire_at = exp (expiration)
/// - signature = JWT signature
#[derive(Clone)]
pub struct Token {
    /// Opaque, non-enumerable reference to kernel object
    ///
    /// Security properties:
    /// - Cannot enumerate to discover objects
    /// - Cannot forge or guess valid scopes
    /// - Kernel-internal mapping to actual objects
    pub scope: OpaqueScope,

    /// Rights mask (NOT role-based access control)
    ///
    /// Explicit bit-level permissions prevent privilege confusion.
    pub role: Rights,

    /// Identifies delegation domain
    ///
    /// - `Issuer::Kernel` = kernel-minted root authority
    /// - `Issuer::Authority(id)` = userspace service-minted derived token
    pub issuer: Issuer,

    /// Mandatory expiration timestamp
    ///
    /// All serialized tokens MUST have expiration.
    /// Benefits:
    /// - Bounds replay window
    /// - Reduces need for revocation lists
    /// - Forces re-authentication for long-lived access
    pub expire_at: Timestamp,

    /// HMAC signature binding all authorization-relevant fields
    ///
    /// signature = HMAC-SHA256(scope || role || issuer || expire_at, kernel_secret)
    pub signature: Signature,
}

impl Token {
    /// Create a new token with signature
    ///
    /// # Arguments
    ///
    /// * `scope` - Opaque reference to kernel object
    /// * `role` - Rights bitmask
    /// * `issuer` - Who is creating this token
    /// * `expire_at` - When this token expires
    /// * `secret` - Kernel secret key for signing
    ///
    /// # Returns
    ///
    /// Token with valid signature
    pub fn new(
        scope: OpaqueScope,
        role: Rights,
        issuer: Issuer,
        expire_at: Timestamp,
        secret: &[u8; 32],
    ) -> Self {
        let signature = Signature::compute(scope, role, issuer, expire_at, secret);

        Self {
            scope,
            role,
            issuer,
            expire_at,
            signature,
        }
    }

    /// Verify token signature
    ///
    /// Returns true if signature is valid (token hasn't been tampered with).
    pub fn verify(&self, secret: &[u8; 32]) -> bool {
        let expected = Signature::compute(self.scope, self.role, self.issuer, self.expire_at, secret);
        self.signature.constant_time_eq(&expected)
    }

    /// Check if token is expired
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now >= self.expire_at
    }

    /// Check if token has a specific right
    pub fn has_right(&self, right: Rights) -> bool {
        self.role.contains(right)
    }

    /// Derive a new token with reduced rights
    ///
    /// Creates a new token with:
    /// - Same scope
    /// - Subset of rights (never more)
    /// - New issuer (delegation tracking)
    /// - Earlier or same expiration
    ///
    /// # Arguments
    ///
    /// * `new_rights` - Reduced rights (must be subset of current)
    /// * `new_expiry` - New expiration (must be <= current)
    /// * `new_issuer` - New issuer (delegation)
    /// * `secret` - Kernel secret for signing
    ///
    /// # Returns
    ///
    /// * `Some(Token)` if derivation is valid
    /// * `None` if trying to escalate privileges
    pub fn derive(
        &self,
        new_rights: Rights,
        new_expiry: Timestamp,
        new_issuer: Issuer,
        secret: &[u8; 32],
    ) -> Option<Token> {
        // Cannot escalate rights
        if !self.role.contains(new_rights) {
            return None;
        }

        // Cannot extend expiration
        if new_expiry > self.expire_at {
            return None;
        }

        // Cannot derive from expired token
        // (Note: caller should check this with current time)

        Some(Token::new(
            self.scope, new_rights, new_issuer, new_expiry, secret,
        ))
    }
}

impl core::fmt::Debug for Token {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Token")
            .field("scope", &self.scope)
            .field("role", &self.role)
            .field("issuer", &self.issuer)
            .field("expire_at", &self.expire_at)
            .field("signature", &"<redacted>")
            .finish()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Invoke Operations
// ═══════════════════════════════════════════════════════════════════════════

/// Operations performed through sys_invoke()
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvokeOp {
    // Thread operations
    ThreadCreate = 0,
    ThreadDestroy = 1,
    ThreadSuspend = 2,
    ThreadResume = 3,
    ThreadSetPriority = 4,

    // Space operations
    SpaceCreate = 10,
    SpaceDestroy = 11,
    SpaceMap = 12,
    SpaceUnmap = 13,
    SpaceGrant = 14,
    SpaceMapRange = 15, // Batch mapping for multiple pages

    // Token operations
    TokenDerive = 20,
    TokenRevoke = 21,

    // IRQ operations
    IrqAttach = 30,
    IrqAck = 31,

    // IPC operations
    EndpointCreate = 40,

    // PCI operations
    PciConfigRead = 50,
    PciConfigWrite = 51,

    // I/O port operations
    PortIn8 = 52,
    PortIn16 = 53,
    PortIn32 = 54,
    PortOut8 = 55,
    PortOut16 = 56,
    PortOut32 = 57,

    // Memory translation
    VirtToPhys = 58,
    PmmAllocLarge = 59,

    // Clock/time
    ClockNow = 60,
}

impl InvokeOp {
    pub fn from_usize(n: usize) -> Option<Self> {
        match n {
            0 => Some(Self::ThreadCreate),
            1 => Some(Self::ThreadDestroy),
            2 => Some(Self::ThreadSuspend),
            3 => Some(Self::ThreadResume),
            4 => Some(Self::ThreadSetPriority),
            10 => Some(Self::SpaceCreate),
            11 => Some(Self::SpaceDestroy),
            12 => Some(Self::SpaceMap),
            13 => Some(Self::SpaceUnmap),
            14 => Some(Self::SpaceGrant),
            15 => Some(Self::SpaceMapRange),
            20 => Some(Self::TokenDerive),
            21 => Some(Self::TokenRevoke),
            30 => Some(Self::IrqAttach),
            31 => Some(Self::IrqAck),
            40 => Some(Self::EndpointCreate),
            50 => Some(Self::PciConfigRead),
            51 => Some(Self::PciConfigWrite),
            52 => Some(Self::PortIn8),
            53 => Some(Self::PortIn16),
            54 => Some(Self::PortIn32),
            55 => Some(Self::PortOut8),
            56 => Some(Self::PortOut16),
            57 => Some(Self::PortOut32),
            58 => Some(Self::VirtToPhys),
            59 => Some(Self::PmmAllocLarge),
            60 => Some(Self::ClockNow),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_handle() {
        let handle = TokenHandle::new(42);
        assert_eq!(handle.as_usize(), 42);
    }

    #[test]
    fn test_timestamp_ordering() {
        let t1 = Timestamp::new(100);
        let t2 = Timestamp::new(200);
        assert!(t1 < t2);
        assert_eq!(t1, Timestamp::new(100));
    }

    #[test]
    fn test_issuer_serialization() {
        let kernel = Issuer::Kernel;
        assert_eq!(kernel.to_bytes(), [0u8; 8]);

        let authority = Issuer::Authority(AuthorityId::new(42));
        assert_eq!(authority.to_bytes(), 42u64.to_le_bytes());
    }

    #[test]
    fn test_invoke_op_conversion() {
        assert_eq!(InvokeOp::from_usize(0), Some(InvokeOp::ThreadCreate));
        assert_eq!(InvokeOp::from_usize(12), Some(InvokeOp::SpaceMap));
        assert_eq!(InvokeOp::from_usize(999), None);
    }
}
