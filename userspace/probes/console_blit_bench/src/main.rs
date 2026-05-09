//! Microbench: write 100 × 25 lines via stdout (TTY → console → framebuffer)
//! and emit a cycles_per_full_screen marker for the b_console_blit harness.
//!
//! NOTE: debug_print (kernel syscall 255) writes only to COM2/serial and does NOT
//! exercise the framebuffer pipeline.  We therefore use `ipc::send_with_payload` on
//! the stdout token (TTY_WRITE_LABEL=2) so every write travels through:
//!   probe → TTY IPC → console process → framebuffer blit (WC-mapped MMIO)
//! This is the path affected by the MAP_DEVICE_WC change and is the correct one to
//! ratchet.  The start/end debug_print lines are COM2-only and exist solely so the
//! harness can anchor on them in the serial log.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::ipc::{send_with_payload, TTY_WRITE_LABEL};

const ITERS: u64 = 100;
const LINE: &[u8] = b"The quick brown fox jumps over the lazy dog 0123456789!@#\n";

#[inline]
fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "mfence",
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nostack, preserves_flags),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = libcluu::debug_print("BENCH_CONSOLE_BLIT: start");

    let stdout = libcluu::boot::stdout();

    let start = rdtsc();
    for _ in 0..ITERS {
        for _ in 0..25 {
            let _ = send_with_payload(stdout, TTY_WRITE_LABEL, LINE);
        }
    }
    let elapsed = rdtsc().wrapping_sub(start);

    let cycles_per_iter = elapsed / ITERS;
    let marker = format!(
        "BENCH_CONSOLE_BLIT: cycles_per_full_screen={} iters={} total_cycles={}",
        cycles_per_iter, ITERS, elapsed
    );
    // Emit via debug_print so harness can read from serial log
    let _ = libcluu::debug_print(&marker);
    // Also write to stdout so it appears on the framebuffer
    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, marker.as_bytes());
    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"\n");
    0
}
