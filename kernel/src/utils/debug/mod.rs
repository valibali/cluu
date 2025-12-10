/*
 * Debugging and Logging Utilities
 *
 * This module contains utilities for debugging and logging,
 * providing structured logging and debug output capabilities.
 */

pub mod logger;

/// Initialize debug infrastructure (COM2 port for logging)
pub fn init_debug_infrastructure() {
    // Initialize COM2 port for debug/logging output
    crate::drivers::serial::init_debug_port();
}
