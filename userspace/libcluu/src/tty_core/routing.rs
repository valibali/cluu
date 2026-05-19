//! Service-side input-routing helper.
//!
//! Both `userspace/tty/` and `userspace/cluuterm/` call this to translate
//! a `LineDiscOutput` into a list of `ServiceAction`s the service then
//! executes.

use crate::tty_core::line_discipline::{LineDiscOutput, SignalNum};

/// Concrete action a PTS service must take after line-discipline processing.
#[derive(Clone, Debug)]
pub enum ServiceAction {
    /// Deliver cooked bytes to any blocked PTS_READ caller.
    DeliverBytes(alloc::vec::Vec<u8>),
    /// Send a signal to the foreground process group.
    SignalFgPgrp(SignalNum),
    /// Write echo bytes back to the rendering layer (cluuterm grid /
    /// tty framebuffer).
    Echo(alloc::vec::Vec<u8>),
    /// EOF reached; deliver EOF marker to readers.
    DeliverEof,
}

/// Translate one `LineDiscOutput` event into a `ServiceAction`.
pub fn translate_output(ev: LineDiscOutput) -> Option<ServiceAction> {
    match ev {
        LineDiscOutput::Bytes(b)   => Some(ServiceAction::DeliverBytes(b)),
        LineDiscOutput::Signal(s)  => Some(ServiceAction::SignalFgPgrp(s)),
        LineDiscOutput::Echo(b)    => Some(ServiceAction::Echo(b)),
        LineDiscOutput::Eof        => Some(ServiceAction::DeliverEof),
        LineDiscOutput::Drop       => None,
    }
}

/// Convenience: feed one byte, return a (possibly empty) action list.
pub fn route_input_byte(
    ld: &mut crate::tty_core::line_discipline::LineDiscipline,
    byte: u8,
) -> alloc::vec::Vec<ServiceAction> {
    ld.feed_byte(byte).into_iter().filter_map(translate_output).collect()
}