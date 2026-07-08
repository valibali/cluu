#![no_std]
#![allow(dead_code)]

extern crate alloc;

pub mod dynamic;
pub mod reloc;
pub mod tls;

pub use dynamic::{DynamicInfo, DynTag};
pub use reloc::{apply_relocs, RelocError, RelocResult};
pub use tls::{TlsBlock, TlsModule, __tls_get_addr, init_thread_tls};
