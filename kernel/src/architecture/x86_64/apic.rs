//! Local APIC (xAPIC) support
//!
//! Provides basic Local APIC enablement and timer programming.
//! This is intentionally minimal: no IOAPIC yet, just the LAPIC timer.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::mm;
use x86_64::registers::model_specific::Msr;

const IA32_APIC_BASE_MSR: u32 = 0x1B;
const APIC_BASE_MASK: u64 = 0xFFFF_F000;
const APIC_ENABLE_BIT: u64 = 1 << 11;

const APIC_MMIO_VIRT_BASE: u64 = crate::mm::space::layout::KERNEL_APIC_BASE;

const APIC_REG_EOI: u32 = 0x0B0;
const APIC_REG_SVR: u32 = 0x0F0;
const APIC_REG_LVT_TIMER: u32 = 0x320;
const APIC_REG_TIMER_DIVIDE: u32 = 0x3E0;
const APIC_REG_TIMER_INIT_COUNT: u32 = 0x380;

const APIC_SVR_ENABLE: u32 = 1 << 8;
const APIC_TIMER_MODE_PERIODIC: u32 = 1 << 17;

const APIC_TIMER_VECTOR: u8 = 32;
const APIC_SPURIOUS_VECTOR: u8 = 0x68;

const APIC_TIMER_DIVIDE: u32 = 16;
const APIC_REG_TIMER_CUR_COUNT: u32 = 0x390;
const APIC_LVT_MASKED: u32 = 1 << 16;

static APIC_ENABLED: AtomicBool = AtomicBool::new(false);
static APIC_MMIO_BASE: AtomicU64 = AtomicU64::new(0);

pub fn is_enabled() -> bool {
    APIC_ENABLED.load(Ordering::Acquire)
}

pub fn eoi() {
    if !is_enabled() {
        return;
    }
    unsafe {
        write(APIC_REG_EOI, 0);
    }
}

pub fn init_timer(hz: u32) -> Result<(), &'static str> {
    let phys_base = enable_local_apic()?;
    unsafe {
        map_mmio(phys_base)?;
        program_timer(hz);
    }
    APIC_ENABLED.store(true, Ordering::Release);
    Ok(())
}

fn enable_local_apic() -> Result<u64, &'static str> {
    let mut msr = Msr::new(IA32_APIC_BASE_MSR);
    let mut value = unsafe { msr.read() };
    value |= APIC_ENABLE_BIT;
    unsafe {
        msr.write(value);
    }
    Ok(value & APIC_BASE_MASK)
}

unsafe fn map_mmio(phys_base: u64) -> Result<(), &'static str> {
    let virt = APIC_MMIO_VIRT_BASE;
    mm::vmm::map_kernel_mmio(virt, phys_base)?;
    APIC_MMIO_BASE.store(virt, Ordering::Release);
    Ok(())
}

unsafe fn program_timer(hz: u32) {
    let divider = match APIC_TIMER_DIVIDE {
        1 => 0b1011,
        2 => 0b0000,
        4 => 0b0001,
        8 => 0b0010,
        16 => 0b0011,
        32 => 0b1000,
        64 => 0b1001,
        128 => 0b1010,
        _ => 0b0011,
    };

    write(APIC_REG_TIMER_DIVIDE, divider);

    let ticks_per_sec = calibrate_apic_timer_hz().unwrap_or(1_000_000_000 / APIC_TIMER_DIVIDE);
    let initial = (ticks_per_sec / hz.max(1)).max(1);

    // Mask timer while calibrating/programming to avoid stray IRQs.
    write(
        APIC_REG_LVT_TIMER,
        APIC_LVT_MASKED | (APIC_TIMER_VECTOR as u32),
    );
    write(APIC_REG_TIMER_INIT_COUNT, 0);
    write(
        APIC_REG_LVT_TIMER,
        APIC_TIMER_MODE_PERIODIC | (APIC_TIMER_VECTOR as u32),
    );
    write(
        APIC_REG_SVR,
        APIC_SVR_ENABLE | (APIC_SPURIOUS_VECTOR as u32),
    );
    write(APIC_REG_TIMER_INIT_COUNT, initial);

    klibcluu::info("APIC timer enabled");
}

unsafe fn calibrate_apic_timer_hz() -> Option<u32> {
    let tsc_hz = crate::architecture::x86_64::tsc::tsc_hz();
    if tsc_hz == 0 {
        return None;
    }

    // 10ms window.
    let tsc_delta_target = tsc_hz / 100;
    if tsc_delta_target == 0 {
        return None;
    }

    write(
        APIC_REG_LVT_TIMER,
        APIC_LVT_MASKED | (APIC_TIMER_VECTOR as u32),
    );
    write(APIC_REG_TIMER_INIT_COUNT, u32::MAX);
    let start = crate::architecture::x86_64::tsc::rdtsc();
    while crate::architecture::x86_64::tsc::rdtsc().saturating_sub(start) < tsc_delta_target {
        core::hint::spin_loop();
    }
    let cur = read(APIC_REG_TIMER_CUR_COUNT);
    let elapsed = u32::MAX.saturating_sub(cur);
    if elapsed == 0 {
        return None;
    }
    let per_sec = (elapsed as u64).saturating_mul(100);
    if per_sec == 0 || per_sec > u32::MAX as u64 {
        return None;
    }
    Some(per_sec as u32)
}

unsafe fn write(offset: u32, value: u32) {
    let base = APIC_MMIO_BASE.load(Ordering::Acquire);
    let addr = (base + offset as u64) as *mut u32;
    core::ptr::write_volatile(addr, value);
}

unsafe fn read(offset: u32) -> u32 {
    let base = APIC_MMIO_BASE.load(Ordering::Acquire);
    let addr = (base + offset as u64) as *const u32;
    core::ptr::read_volatile(addr)
}
