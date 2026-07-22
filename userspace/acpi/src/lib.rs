#![no_std]
#![allow(dead_code)]

extern crate alloc;

pub mod rsdp;
pub mod fadt;
pub mod mcfg;
pub mod tables;
pub mod discovery;
pub mod dsdt;

pub use rsdp::{find_rsdp, find_rsdp_with_phys, Rsdp};
pub use fadt::Fadt;
pub use mcfg::{Mcfg, McfgEntry};
pub use tables::{SdtHeader, SdtSignature};
pub use discovery::{
    find_dsdt_bytes, find_fadt_from_phys, find_fadt_from_rsdp, find_mcfg_from_rsdp,
    find_ssdt_bytes_from_rsdp,
};
pub use dsdt::{parse_devices, AcpiDevice};
