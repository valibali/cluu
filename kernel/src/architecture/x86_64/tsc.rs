//! TSC helpers and boot-time calibration.
//!
//! Uses PIT channel 2 to estimate TSC frequency during boot.

use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::instructions::port::Port;

const PIT_HZ: u64 = 1_193_182;
const PIT_CMD_PORT: u16 = 0x43;
const PIT_CH2_PORT: u16 = 0x42;
const PIT_GATE_PORT: u16 = 0x61;

// ~50ms measurement window, safely below 16-bit PIT reload limit.
const PIT_CALIBRATE_COUNT: u16 = 59_659;
const TSC_HZ_FALLBACK: u64 = 1_000_000_000;

static TSC_HZ: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    }
    ((hi as u64) << 32) | (lo as u64)
}

pub fn calibrate() -> u64 {
    let measured = calibrate_with_pit().unwrap_or(TSC_HZ_FALLBACK);
    TSC_HZ.store(measured, Ordering::Release);
    measured
}

#[inline]
pub fn tsc_hz() -> u64 {
    let hz = TSC_HZ.load(Ordering::Acquire);
    if hz == 0 {
        TSC_HZ_FALLBACK
    } else {
        hz
    }
}

fn calibrate_with_pit() -> Option<u64> {
    // Median of three runs to reduce jitter from SMI/VM exits.
    let mut samples = [0u64; 3];
    for sample in &mut samples {
        *sample = calibrate_once()?;
    }
    samples.sort_unstable();
    Some(samples[1])
}

fn calibrate_once() -> Option<u64> {
    let mut pit_cmd = Port::<u8>::new(PIT_CMD_PORT);
    let mut pit_ch2 = Port::<u8>::new(PIT_CH2_PORT);
    let mut pit_gate = Port::<u8>::new(PIT_GATE_PORT);

    let gate = unsafe { pit_gate.read() };
    // Disable speaker data (bit 1), drop gate (bit 0) for a clean restart.
    unsafe { pit_gate.write((gate & !0x03) | 0x00) };

    // Channel 2, lobyte/hibyte, mode 0 (one-shot), binary.
    unsafe { pit_cmd.write(0xB0) };
    unsafe { pit_ch2.write((PIT_CALIBRATE_COUNT & 0xFF) as u8) };
    unsafe { pit_ch2.write((PIT_CALIBRATE_COUNT >> 8) as u8) };

    // Start counting: gate high, speaker still disabled.
    let gate_now = unsafe { pit_gate.read() };
    unsafe { pit_gate.write((gate_now & !0x02) | 0x01) };

    let start = rdtsc();
    // OUT2 (bit 5) goes high when terminal count is reached.
    loop {
        let status = unsafe { pit_gate.read() };
        if (status & 0x20) != 0 {
            break;
        }
        core::hint::spin_loop();
    }
    let end = rdtsc();
    let delta = end.saturating_sub(start);
    if delta == 0 {
        return None;
    }

    let hz = ((delta as u128) * (PIT_HZ as u128) / (PIT_CALIBRATE_COUNT as u128)) as u64;
    if hz < 10_000_000 {
        return None;
    }
    Some(hz)
}
