//! Page Attribute Table (PAT) programming.
//!
//! Linux-compatible layout:
//!   PAT[0] = WB   (PCD=0, PWT=0)  — default cached
//!   PAT[1] = WC   (PCD=0, PWT=1)  — write-combining (this is the new entry)
//!   PAT[2] = UC-  (PCD=1, PWT=0)  — current `NO_CACHE` device mapping
//!   PAT[3] = UC   (PCD=1, PWT=1)  — strict uncached
//!   PAT[4..8] mirror PAT[0..4] (PAT bit unused on 4K pages here, kept benign).
//!
//! Existing PTEs that set only PCD continue to map to UC- (PAT[2]) — no
//! behavior change. New WC mappings select index 1 by setting PWT only.
//!
//! TODO(SMP): every AP must call `pat::init()` from its AP-init path. Today
//! the kernel is UP, so a single BSP call is sufficient.

const IA32_PAT: u32 = 0x277;

const PA_WB:  u64 = 0x06;
const PA_WC:  u64 = 0x01;
const PA_UCM: u64 = 0x07; // UC-
const PA_UC:  u64 = 0x00;

const PAT_VALUE: u64 =
    (PA_WB  <<  0) |
    (PA_WC  <<  8) |
    (PA_UCM << 16) |
    (PA_UC  << 24) |
    (PA_WB  << 32) |
    (PA_WC  << 40) |
    (PA_UCM << 48) |
    (PA_UC  << 56);

/// Program the PAT MSR on the current CPU. Must be called once per CPU at boot,
/// before any user-visible mapping that relies on the new layout. Reloads CR3
/// after `wrmsr` to flush TLB-cached PAT-derived entries (Intel SDM 12.11.8).
pub fn init() {
    unsafe {
        let lo = (PAT_VALUE & 0xFFFF_FFFF) as u32;
        let hi = (PAT_VALUE >> 32) as u32;
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_PAT,
            in("eax") lo,
            in("edx") hi,
            options(nostack, preserves_flags),
        );

        let cr3: u64;
        core::arch::asm!("mov {0}, cr3", out(reg) cr3, options(nomem, preserves_flags));
        core::arch::asm!("mov cr3, {0}", in(reg) cr3, options(nostack, preserves_flags));
    }
}

/// Read back the PAT MSR (debug/sanity helper).
#[allow(dead_code)]
pub fn read() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") IA32_PAT,
            out("eax") lo,
            out("edx") hi,
            options(nomem, preserves_flags),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Expected MSR value for boot-time verification.
pub fn expected() -> u64 { PAT_VALUE }
