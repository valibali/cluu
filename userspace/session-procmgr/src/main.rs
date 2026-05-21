#![cfg_attr(not(feature = "host-test"), no_std)]
#![cfg_attr(not(feature = "host-test"), no_main)]
extern crate alloc;

/// Production kernel adapter — compiled only for target (real x86-64 syscalls).
#[cfg(not(feature = "host-test"))]
mod real_kernel;

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    // Bootstrap implemented in later phases.
    0
}
