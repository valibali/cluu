/*
 * System Call and Low-Level I/O Interface
 *
 * This module provides the foundation for system calls and low-level I/O
 * operations in the kernel. It contains abstractions for different types
 * of I/O interfaces and will eventually support user-space system calls.
 *
 * Why this is important:
 * - Provides safe abstractions for hardware I/O operations
 * - Forms the basis for future system call implementation
 * - Enables type-safe access to hardware registers and ports
 * - Provides consistent interface across different I/O methods
 * - Essential for all hardware interaction in the kernel
 *
 * Current modules:
 * - io: Generic I/O trait and wrapper types
 * - pio: Port I/O implementation for x86 architecture
 */

pub mod io;
pub mod pio;
