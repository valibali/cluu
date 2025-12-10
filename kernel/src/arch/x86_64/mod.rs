/*
 * x86_64 Architecture Support Module
 *
 * This module contains all x86_64-specific code for the CLUU kernel.
 * It provides the low-level architecture support needed for proper
 * kernel operation on x86_64 processors.
 *
 * Why this is important:
 * - Encapsulates all architecture-specific functionality
 * - Provides clean separation between generic kernel code and x86_64 specifics
 * - Enables potential porting to other architectures in the future
 * - Contains critical system initialization code
 * - Manages CPU-specific features and capabilities
 *
 * Submodules:
 * - gdt: Global Descriptor Table management
 * - idt: Interrupt Descriptor Table and exception handling
 * - interrupts: Interrupt control utilities
 * - peripheral: Hardware device drivers and interfaces
 */

pub mod gdt;
pub mod idt;
pub mod interrupts;

