/*
 * Architecture Abstraction Layer
 *
 * This module provides an abstraction layer over different CPU architectures,
 * currently supporting x86_64. It contains the main kernel initialization
 * sequence and architecture-specific setup code.
 *
 * Why this is important:
 * - Provides a clean interface between generic kernel code and arch-specific code
 * - Enables potential future support for other architectures (ARM, RISC-V, etc.)
 * - Contains the critical kernel startup sequence
 * - Manages hardware initialization in the correct order
 * - Provides centralized architecture detection and setup
 *
 * The kstart() function is the main kernel entry point after the initial
 * assembly bootstrap, responsible for initializing all kernel subsystems
 * in the proper order.
 */

#[cfg(target_arch = "x86_64")]
#[macro_use]
pub mod x86_64;
