//! CLUU Microkernel Library
//!
//! This module exists to allow unit testing of kernel components

#![no_std]
#![cfg_attr(test, allow(unused))]

// Module structure will be built out incrementally
pub mod error;

// Re-exports
pub use error::Error;
