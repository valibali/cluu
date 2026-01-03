//! CLUU Userspace Library
//!
//! Provides syscall wrappers, IPC helpers, and runtime support for userspace programs

#![no_std]
#![cfg_attr(not(feature = "std"), no_main)]

pub mod syscall;
pub mod ipc;
pub mod types;
pub mod runtime;

// Re-exports
pub use types::*;
pub use ipc::{send, recv, call, reply};
