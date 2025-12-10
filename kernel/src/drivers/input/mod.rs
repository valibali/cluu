/*
 * Input Device Drivers
 *
 * This module contains drivers for input devices such as
 * keyboards, mice, and other human interface devices.
 */

pub mod keyboard;

/// Initialize input devices
pub fn init() {
    // Keyboard driver is initialized through interrupt handlers
    log::info!("Keyboard driver ready");
}
