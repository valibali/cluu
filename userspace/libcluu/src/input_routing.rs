// userspace/libcluu/src/input_routing.rs
//! Shared types for input-routing IPC.
//!
//! Router today is vtmgr; tomorrow it will be a dedicated inputd.
//! These types live in libcluu so both ends speak the same dialect
//! regardless of which process is the publisher.

#![allow(dead_code)]

/// Where keystrokes should go for the currently-active VT.
///
/// Used internally by the router (vtmgr today) to pick which output
/// send-token to use for an incoming event. NOT serialised on the
/// wire — the router holds the token table directly.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RoutingTargetKind {
    /// No active target yet (boot, quiesce, transition). Router drops events.
    None,
    /// Forward to the compositor's input endpoint.
    Compositor,
    /// Forward to tty:N's main endpoint. N is the VT index (0..=3).
    Tty(u8),
}
