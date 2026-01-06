//! Userspace runtime - entry point and panic handler

#[cfg(all(not(feature = "std"), not(test)))]
use core::panic::PanicInfo;

#[cfg(all(not(feature = "std"), not(test)))]
use crate::syscall::thread_exit;

/// Entry point for userspace programs
///
/// This is called by the kernel after loading the program.
/// It sets up the runtime and calls main().
#[cfg(all(not(feature = "std"), not(test), target_os = "none"))]
#[no_mangle]
#[link_section = ".text._start"]
pub extern "C" fn _start() -> ! {
    extern "Rust" {
        fn main() -> i32;
    }

    let exit_code = unsafe { main() };
    thread_exit(exit_code);
}

/// Panic handler for userspace
///
/// Note: This is only active when building for the target (not during testing)
#[cfg(all(not(feature = "std"), not(test)))]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // TODO: Print panic info via syscall
    thread_exit(-1);
}
