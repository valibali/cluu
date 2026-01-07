#[cfg(target_arch = "x86_64")]
#[macro_use]
pub mod x86_64;

// NOTE: kstart() has been replaced by _start() in main.rs
// The old kstart() depended on peripheral module (framebuffer)
// which will be moved to userspace later.
//
// New boot sequence in main.rs:
// 1. UART init (COM2 at 0x2F8)
// 2. Logger init (IRQ-safe)
// 3. GDT init
// 4. IDT init
// 5. Syscall init
// 6. Idle loop
