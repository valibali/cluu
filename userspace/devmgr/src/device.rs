//! Domain types for the device registry.
//!
//! Pure data — no IPC, no syscalls. Separated from `registry.rs` so the
//! types can be referenced without pulling in the full registry logic.

extern crate alloc;

use alloc::string::String;

pub type DeviceId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceClass {
    Block = 0,
    Char = 1,
    Input = 2,
    Framebuffer = 3,
}

impl DeviceClass {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Block),
            1 => Some(Self::Char),
            2 => Some(Self::Input),
            3 => Some(Self::Framebuffer),
            _ => None,
        }
    }

    pub fn as_region_kind(self) -> u8 {
        self as u8
    }
}

pub struct DeviceEntry {
    pub class: DeviceClass,
    pub driver_endpoint: usize,
    pub path: String,
    pub root_token: usize,
    pub total_sectors: u64,
}

impl DeviceEntry {
    pub fn new_block(
        path: String,
        driver_endpoint: usize,
        root_token: usize,
        total_sectors: u64,
    ) -> Self {
        Self {
            class: DeviceClass::Block,
            driver_endpoint,
            path,
            root_token,
            total_sectors,
        }
    }

    pub fn new_char(
        class: DeviceClass,
        path: String,
        driver_endpoint: usize,
        root_token: usize,
    ) -> Self {
        Self {
            class,
            driver_endpoint,
            path,
            root_token,
            total_sectors: 0,
        }
    }
}
