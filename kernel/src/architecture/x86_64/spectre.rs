//! Spectre V2 mitigations: IBPB on cross-address-space context switch, STIBP at boot.

use core::sync::atomic::{AtomicBool, Ordering};

static HAS_IBPB: AtomicBool = AtomicBool::new(false);
static HAS_STIBP: AtomicBool = AtomicBool::new(false);

/// Call once during boot after CPUID leaf 7/0.
pub fn init(cpuid7_edx: u32) {
    let ibpb = (cpuid7_edx & (1 << 26)) != 0;
    let stibp = (cpuid7_edx & (1 << 27)) != 0;

    HAS_IBPB.store(ibpb, Ordering::Release);
    HAS_STIBP.store(stibp, Ordering::Release);

    if ibpb {
        klibcluu::info("  IBPB supported (CPUID.7.0:EDX[26])");
    }
    if stibp {
        klibcluu::info("  STIBP supported (CPUID.7.0:EDX[27])");
        unsafe { set_stibp(); }
    }
}

/// Issue IBPB — flush indirect branch predictor.
#[inline(always)]
pub unsafe fn ibpb() {
    core::arch::asm!(
        "xor edx, edx",
        "mov eax, 1",
        "mov ecx, 0x49",
        "wrmsr",
        options(nomem, nostack),
    );
}

/// Set STIBP bit in IA32_SPEC_CTRL (MSR 0x48, bit 1).
unsafe fn set_stibp() {
    let lo: u32;
    let hi: u32;
    core::arch::asm!(
        "mov ecx, 0x48",
        "rdmsr",
        out("eax") lo,
        out("edx") hi,
        out("ecx") _,
        options(nomem, nostack),
    );
    let new_lo = lo | (1 << 1);
    core::arch::asm!(
        "mov ecx, 0x48",
        "wrmsr",
        in("eax") new_lo,
        in("edx") hi,
        out("ecx") _,
        options(nomem, nostack),
    );
    klibcluu::info("  STIBP set in IA32_SPEC_CTRL");
}

#[inline(always)]
pub fn has_ibpb() -> bool {
    HAS_IBPB.load(Ordering::Relaxed)
}
