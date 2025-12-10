/*
 * Kernel Utilities and Support Functions
 *
 * This module contains various utility functions, macros, and support
 * code used throughout the kernel. It provides common functionality
 * like logging, text output, and debugging macros.
 *
 * Why this is important:
 * - Provides essential debugging and logging infrastructure
 * - Implements kernel-specific versions of common operations
 * - Enables consistent formatting and output across the kernel
 * - Provides macros for simplified kernel development
 * - Forms the support infrastructure for kernel debugging
 *
 * Key components:
 * - writer: Serial port text output functionality
 * - macros: Kernel-specific print and debug macros
 * - logger: Structured logging system for kernel messages
 */

pub mod writer;
#[macro_use]
pub mod macros;
pub mod logger;
pub mod timer;
pub mod console;
pub mod line_editor;
pub mod shell;
pub mod reboot;
