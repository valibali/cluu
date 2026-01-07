//! Process Manager Library
//!
//! This library provides the core functionality for process management,
//! including ELF parsing and loading.

#![no_std]

pub use elf::{ElfFile, LoadableSegment};
pub use libcluu::elf;
