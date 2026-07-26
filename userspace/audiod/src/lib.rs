//! CLUU audio daemon — pure mixer/resampler/ring core.
//!
//! Host-testable pure logic: ring buffer, linear resampler, N-stream mixer,
//! session/stream state machine. No IPC, no SHM, no hardware dependencies.
//! The `main.rs` binary wires these into the no_std userspace daemon.

#![no_std]

extern crate alloc;

pub mod ring;
pub mod resample;
pub mod mixer;
pub mod session;
