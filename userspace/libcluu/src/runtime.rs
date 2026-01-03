//! Userspace runtime - entry point and panic handler

use core::panic::PanicInfo;
use crate::syscall::sys_thread_exit;

/// Entry point for userspace programs
///
/// This is called by the kernel after loading the program.
/// It sets up the runtime and calls main().
#[no_mangle]
#[link_section = ".text._start"]
pub extern "C" fn _start() -> ! {
    extern "Rust" {
        fn main() -> i32;
    }

    let exit_code = unsafe { main() };
    sys_thread_exit(exit_code);
}

/// Panic handler for userspace
#[cfg(not(feature = "std"))]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // TODO: Print panic info via syscall
    sys_thread_exit(-1);
}
