/*
 * System Controller Drivers
 *
 * This module contains drivers for system-level hardware controllers
 * such as interrupt controllers, timers, and other system management devices.
 */

pub mod pic;

/// Initialize system controllers (PIC and PIT)
pub fn init() {
    pic::init_pic();
    log::info!("PIC initialization complete");
    log::info!("Initializing PIT (100 Hz)...");
    pic::init_pit(100);
    log::info!("PIT initialization complete");
}
