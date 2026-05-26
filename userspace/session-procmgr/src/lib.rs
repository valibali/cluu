#![cfg_attr(not(feature = "host-test"), no_std)]
extern crate alloc;

pub mod cap_broker_session;
pub mod child_monitor;
pub mod child_table;
pub mod ctty;
pub mod dispatch;
pub mod kill;
pub mod pg_table;
pub mod pipe_handlers;
pub mod pipe_registry;
pub mod proc_query_local;
pub mod restart;
pub mod spawn;

/// Production kernel adapter — excluded from host-test (uses real x86-64 syscalls).
#[cfg(not(feature = "host-test"))]
pub mod real_kernel;

/// Production ELF-spawn primitive — excluded from host-test (uses real VFS + syscalls).
#[cfg(not(feature = "host-test"))]
pub mod elf_spawn;
