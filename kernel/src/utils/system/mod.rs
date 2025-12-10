/*
 * System Management Utilities
 *
 * This module contains utilities for system-level operations
 * such as timing, power management, and system control.
 */

pub mod reboot;
pub mod timer;

pub use reboot::reboot;
