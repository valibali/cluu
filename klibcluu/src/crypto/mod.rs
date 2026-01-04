//! Cryptographic Primitives
//!
//! This module provides cryptographic primitives for the token system:
//! - HMAC-SHA256 for token signatures
//! - SHA-256 for hashing
//! - Secure random number generation for opaque scopes
//!
//! # Implementation
//!
//! Currently uses simple implementations suitable for kernel use.
//! Can be replaced with hardware-accelerated versions later.

mod hmac;
mod sha256;
mod random;

pub use hmac::hmac_sha256;
pub use sha256::hash_sha256;
pub use random::{fill_random, init_random};

/// Initialize crypto subsystem
///
/// Must be called during kernel initialization before creating tokens.
pub fn init() {
    init_random();
    crate::info("Cryptographic subsystem initialized");
}
