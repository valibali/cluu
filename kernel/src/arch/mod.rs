#[cfg(target_arch = "x86_64")]
#[macro_use]
pub mod x86_64;

#[cfg(target_arch = "x86_64")]
use self::x86_64::*;

use utils::logger;

/// Starts the kernel.
///
/// # Returns
///
/// This function does not return.
pub fn kstart() -> ! {
    // Initialize devices
    peripheral::init_peripherals();

    logger::init(true); // Init the logger engine, with clearing the screen

    // Check if framebuffer is available and print "hello"
    if let Some(ref mut fb) = *peripheral::FB.lock() {
        fb.puts("hello");
    }
    
    loop {}
}
