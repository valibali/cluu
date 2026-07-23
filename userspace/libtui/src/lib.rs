//! libtui — Elm-style MVU (Model-View-Update) runtime for CLUU TUI apps.
//!
//! Provides:
//! - `Model` trait: init/update/view interface for TUI applications
//! - `Cmd` type: lazily-evaluated side-effect descriptor
//! - `View` / `Cell`: renderable grid of styled characters
//! - `layout`: Rect, Block, Border, Constraint — layout primitives
//! - Input decoder: raw TTY bytes -> `KeyEvent`
//! - `Program`: event loop (raw mode, alt-screen, read/update/render)
//! - Components: viewport, textinput, list, browser, progress
//!
//! ## Module organization (SOLID)
//!
//! - [`buffer`]: `Cell`, `View`, color/attr constants — pure grid data (SRP)
//! - [`mvu`]: `Model` trait, `Cmd` enum — state/transition contract (SRP)
//! - [`layout`]: `Rect`, `Block`, `Border`, `Constraint`, `Drawable` — layout primitives
//! - [`input`]: `KeyEvent`, `decode` — input decoding (SRP)
//! - [`render`]: CSI emission, `Renderer` — output (SRP)
//! - [`diff`]: `ScreenBuffer` — dirty tracking + diff rendering (SRP)
//! - [`style`]: `Style`, `Border` — declarative styling (SRP)
//! - [`components`]: reusable TUI widgets — `Viewport`, `List`, `TextInput`,
//!   `FileBrowser`, `Progress`
//! - [`program`]: `Program<M>` — event loop runtime (SRP, cfg `runtime`)
//!
//! no_std + alloc. Runtime parts (StdinReader, Renderer, Program) depend
//! on libcluu for TTY IPC and POSIX I/O.
//!
//! ## Backward compatibility
//!
//! All types that were previously at the crate root (`Cell`, `View`,
//! `Model`, `Cmd`, `COLOR_*`, `ATTR_*`) are re-exported here via `pub use`.
//! Existing consumers compile without changes.

#![no_std]
extern crate alloc;

pub mod input;
pub mod render;
pub mod diff;
pub mod components;
pub mod style;
pub mod buffer;
pub mod mvu;
pub mod layout;
#[cfg(feature = "runtime")]
pub mod program;

// =========================================================================
// Facade: re-export core types at crate root for backward compatibility
// =========================================================================

pub use buffer::{
    Cell, View,
    COLOR_DEFAULT, COLOR_BLACK, COLOR_RED, COLOR_GREEN, COLOR_YELLOW,
    COLOR_BLUE, COLOR_MAGENTA, COLOR_CYAN, COLOR_WHITE,
    ATTR_NONE, ATTR_BOLD, ATTR_UNDERLINE, ATTR_REVERSE,
};

pub use mvu::{Model, Cmd};
