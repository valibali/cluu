#![cfg_attr(not(feature = "host-test"), no_std)]
#![cfg_attr(not(feature = "host-test"), no_main)]
extern crate alloc;

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    // Bootstrap implemented in later phases.
    0
}
