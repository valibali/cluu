/*
 * Hardware Drivers
 *
 * This module contains all hardware-specific drivers for the kernel.
 * It provides a clean abstraction layer between the kernel and hardware
 * components, enabling proper device management and initialization.
 *
 * Why this is important:
 * - Centralizes all hardware driver code
 * - Provides consistent interfaces for hardware access
 * - Enables proper device initialization and management
 * - Separates hardware-specific code from kernel logic
 * - Allows for easier hardware abstraction and portability
 *
 * Driver categories:
 * - Serial: UART communication drivers
 * - Display: Framebuffer and graphics drivers
 * - Input: Keyboard and mouse drivers
 * - System: PIC, PIT, and other system controllers
 */

pub mod serial;
pub mod display;
pub mod input;
pub mod system;
