#[cfg(target_arch = "x86_64")]
#[macro_use]
pub mod x86_64;

#[cfg(target_arch = "x86_64")]
use self::x86_64::*;

use x86_64::instructions::*;


/// Starts the kernel.
///
/// # Returns
///
/// This function does not return.
pub fn kstart() -> ! {
    // Initialize devices
    peripheral::init_peripherals();

    // Check if framebuffer is available and print "hello"
    if let Some(ref mut fb) = *peripheral::FB.lock() {
        fb.puts("Visible: The framebuffer is correctly mapped.");
        fb.draw_screen_test();
    }
    
    loop {
        hlt();
    }
}
