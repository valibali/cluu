/*
 * Interrupt Management Module
 *
 * This module provides utilities for managing CPU interrupts, including
 * enabling/disabling interrupts and checking interrupt status. It serves
 * as a high-level interface to x86_64 interrupt control instructions.
 *
 * Why this is important:
 * - Provides safe abstractions for interrupt control
 * - Essential for creating atomic sections in kernel code
 * - Enables proper synchronization in multi-threaded environments
 * - Prevents race conditions in critical kernel operations
 * - Forms the basis for all kernel synchronization primitives
 *
 * Functions in this module are used throughout the kernel to ensure
 * data consistency and prevent corruption during critical operations.
 */

use x86_64::instructions::interrupts;

/// Enable interrupts globally
/// 
/// This allows the CPU to respond to hardware interrupts and exceptions.
/// Should only be called after the IDT has been properly initialized.
pub fn enable() {
    interrupts::enable();
}

/// Disable interrupts globally
/// 
/// This prevents the CPU from responding to hardware interrupts.
/// Useful for critical sections where atomicity is required.
pub fn disable() {
    interrupts::disable();
}

/// Check if interrupts are enabled
/// 
/// Returns true if interrupts are currently enabled, false otherwise.
pub fn are_enabled() -> bool {
    interrupts::are_enabled()
}

/// Execute a closure with interrupts disabled
/// 
/// This is useful for creating atomic sections of code that must not
/// be interrupted by hardware events.
pub fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    interrupts::without_interrupts(f)
}
