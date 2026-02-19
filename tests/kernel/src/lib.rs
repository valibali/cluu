//! CLUU Kernel Test Suite
//!
//! This crate contains unit tests for the CLUU microkernel.
//! Tests are separated from the kernel crate to avoid `no_std` conflicts
//! with the test framework.
//!
//! # Organization
//!
//! - `tests/elf_tests.rs` - ELF loader tests
//! - `tests/mm_tests.rs` - Memory management tests
//! - `tests/cap_tests.rs` - Capability system tests
//! - `tests/sched_tests.rs` - Scheduler tests
//! - `tests/ipc_tests.rs` - IPC tests
//!
//! # Running Tests
//!
//! ```bash
//! cd tests/kernel
//! cargo test
//! ```

// Re-export kernel for tests
pub use cluu_kernel;
