/*
 * Kernel Utilities and Support Functions
 *
 * This module contains various utility functions, macros, and support
 * code used throughout the kernel. It provides common functionality
 * organized into logical groups for better maintainability.
 *
 * Why this is important:
 * - Provides essential debugging and logging infrastructure
 * - Implements kernel-specific versions of common operations
 * - Enables consistent formatting and output across the kernel
 * - Provides macros for simplified kernel development
 * - Forms the support infrastructure for kernel debugging
 *
 * Utility categories:
 * - io: Input/output utilities (console, writer, macros)
 * - system: System management utilities (timer, reboot)
 * - ui: User interface utilities (shell, line editor)
 * - debug: Debugging and logging utilities
 */

pub mod debug;
pub mod io;
pub mod system;
pub mod ui;

// Re-export commonly used items for convenience
pub use io::{console, writer};
pub use system::{reboot, timer};
