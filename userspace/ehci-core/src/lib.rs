#![no_std]
#![allow(dead_code)]

extern crate alloc;

pub mod controller;
pub mod queue;
pub mod regs;

pub use controller::{EhciController, MAX_INTR_SLOTS};
pub use queue::{QueueHead, QtD};
pub use regs::EhciRegs;
