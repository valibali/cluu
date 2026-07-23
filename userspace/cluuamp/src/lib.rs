//! CLUUamp — Winamp-style TUI audio player library.
//!
//! no_std + alloc. The binary target (`src/main.rs`) is a thin wrapper
//! around this library. Pure-logic modules (fft, scope, viscolor, layout,
//! widgets) are unit-testable without a TTY or audio hardware.
//!
//! The `runtime` feature gates modules that depend on libcluu (audio
//! playback, model state, view rendering). Without it, only the pure-logic
//! modules are available — this allows `cargo test` to run on the host
//! without CLUU kernel symbols.

#![no_std]
extern crate alloc;

pub mod equalizer;
pub mod fft;
pub mod gain;
pub mod id3;
pub mod layout;
pub mod scope;
pub mod viscolor;
pub mod widgets;

#[cfg(feature = "runtime")]
pub mod mp3_ffi;
#[cfg(feature = "runtime")]
pub mod audio;
#[cfg(feature = "runtime")]
pub mod model;
#[cfg(feature = "runtime")]
pub mod terminal;
#[cfg(feature = "runtime")]
pub mod view;
