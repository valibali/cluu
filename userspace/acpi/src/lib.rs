#![no_std]
#![allow(dead_code)]

extern crate alloc;

pub mod rsdp;
pub mod fadt;
pub mod mcfg;
pub mod tables;
pub mod discovery;

pub use rsdp::{find_rsdp, Rsdp};
pub use fadt::Fadt;
pub use mcfg::{Mcfg, McfgEntry};
pub use tables::{SdtHeader, SdtSignature};
pub use discovery::{find_fadt_from_phys, find_fadt_from_rsdp};
