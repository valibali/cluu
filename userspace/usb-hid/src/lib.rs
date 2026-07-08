#![no_std]
#![allow(dead_code)]

extern crate alloc;

pub mod hid;
pub mod kbd;
pub mod mouse;

pub use kbd::{HidKeyboard, KbdReport};
pub use mouse::{HidMouse, MouseReport};
