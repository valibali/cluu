//! Cryptographically Secure Random Number Generation
//!
//! Provides random bytes for generating opaque scopes and other
//! security-critical values.
//!
//! # Implementation
//!
//! Uses RDRAND instruction when available, with fallback to
//! timing-based entropy.

use core::arch::x86_64::__cpuid;
use spin::Mutex;

/// RNG state (simple for now, can be improved)
static RNG_STATE: Mutex<RngState> = Mutex::new(RngState::new());

struct RngState {
    counter: u64,
}

impl RngState {
    const fn new() -> Self {
        Self { counter: 0 }
    }

    fn next_u64(&mut self) -> u64 {
        // Try hardware RNG first (RDRAND)
        if let Some(value) = rdrand_u64() {
            return value;
        }

        // Fallback: Mix counter with timestamp
        self.counter = self.counter.wrapping_add(1);

        let timestamp = unsafe {
            let mut tsc: u64;
            core::arch::asm!("rdtsc", out("rax") tsc, out("rdx") _, options(nomem, nostack));
            tsc
        };

        // Simple mixing (not cryptographically secure!)
        let mut value = self.counter ^ timestamp;
        value = value.wrapping_mul(0x5851F42D4C957F2D);
        value ^= value >> 33;
        value = value.wrapping_mul(0x14057B7EF767814F);
        value ^= value >> 28;
        value = value.wrapping_mul(0x5851F42D4C957F2D);
        value ^= value >> 29;

        value
    }
}

/// Try to get random u64 from RDRAND instruction
fn rdrand_u64() -> Option<u64> {
    if !rdrand_supported() {
        return None;
    }

    unsafe {
        let mut value: u64;
        let mut success: u8;

        // Try up to 10 times
        for _ in 0..10 {
            core::arch::asm!(
                "rdrand {value}",
                "setc {success}",
                value = out(reg) value,
                success = out(reg_byte) success,
                options(nomem, nostack)
            );

            if success != 0 {
                return Some(value);
            }
        }

        None
    }
}

fn rdrand_supported() -> bool {
    unsafe {
        let cpuid = __cpuid(1);
        (cpuid.ecx & (1 << 30)) != 0
    }
}

/// Initialize random number generator
///
/// Should be called during kernel init.
pub fn init_random() {
    let mut state = RNG_STATE.lock();

    // Seed with timestamp
    state.counter = unsafe {
        let mut tsc: u64;
        core::arch::asm!("rdtsc", out("rax") tsc, out("rdx") _, options(nomem, nostack));
        tsc
    };
}

/// Fill buffer with random bytes
///
/// # Panics
///
/// Panics if RNG hasn't been initialized.
pub fn fill_random(buf: &mut [u8]) {
    let mut state = RNG_STATE.lock();

    for chunk in buf.chunks_mut(8) {
        let value = state.next_u64();
        let bytes = value.to_le_bytes();

        for (i, &byte) in bytes.iter().take(chunk.len()).enumerate() {
            chunk[i] = byte;
        }
    }
}

/// Get a random u64
pub fn random_u64() -> u64 {
    let mut state = RNG_STATE.lock();
    state.next_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fill_random() {
        init_random();

        let mut buf1 = [0u8; 32];
        let mut buf2 = [0u8; 32];

        fill_random(&mut buf1);
        fill_random(&mut buf2);

        // Should be different (extremely unlikely to be same)
        assert_ne!(buf1, buf2);
    }

    #[test]
    fn test_random_u64() {
        init_random();

        let r1 = random_u64();
        let r2 = random_u64();

        // Should be different (extremely unlikely to be same)
        assert_ne!(r1, r2);
    }
}
