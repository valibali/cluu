//! Compositor compile-time constants.
//!
//! Centralising magic numbers here makes it easy to spot related tunables at
//! a glance and prevents the same value being silently duplicated across files.

// ---------------------------------------------------------------------------
// Frame-timing tunables
// ---------------------------------------------------------------------------

/// Minimum milliseconds between successive framebuffer flushes (~60 fps cap).
pub const MIN_FRAME_MS: u64 = 16;

#[allow(dead_code)]
// rationale: clock-bar refresh interval for future status-bar rendering.
pub const CLOCK_PERIOD_MS: u64 = 1000;

// ---------------------------------------------------------------------------
// Framebuffer device
// ---------------------------------------------------------------------------

/// Magic word at offset 0 of the /dev/fb0 geometry header ("FB0\0").
pub const FB_HEADER_MAGIC: u32 = 0x4642_4630;

// ---------------------------------------------------------------------------
// Window chrome colours
// ---------------------------------------------------------------------------

/// Foreground colour index (xterm-256) for the *focused* window's chrome.
/// Index 15 = bright white.
pub const FOCUSED_FG: u8 = 15;

/// Foreground colour index (xterm-256) for an *unfocused* window's chrome.
/// Index 7 = light grey.
pub const PLAIN_FG: u8 = 7;

/// Cell attribute bits (bold) used for the focused window's chrome cells.
/// Bit layout: bit 0 = bold, bit 1 = underline, bit 2 = reverse.
pub const FOCUSED_BOLD_ATTR: u8 = 0b001;
