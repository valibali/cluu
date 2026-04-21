//! SysV ABI preservation sanity check.
//!
//! The T1.5 syscall fast path pushes only R15 on kernel entry and trusts
//! that the dispatched Rust function preserves the remaining SysV
//! callee-saved registers (RBX, RBP, R12, R13, R14). LLVM's x86_64 backend
//! honors this contract for every `extern "C"` function, but the assumption
//! is load-bearing: a violation would silently corrupt user state on every
//! syscall. This boot-time check pins the contract to a runtime invariant
//! so a regression fails loud instead of causing random userspace bugs.
//!
//! The companion NASM stub `sysv_abi_preservation_test` lives in
//! `syscall_entry.asm`. It loads sentinels into the callee-saved registers,
//! calls `abi_check_callee` here, and verifies the sentinels on return.

extern "C" {
    fn sysv_abi_preservation_test() -> u64;
}

/// Callee exercised by the NASM test. Does enough register-pressure work
/// that LLVM will allocate callee-saved registers — if the SysV contract
/// were broken, the sentinels loaded by the caller would be overwritten.
///
/// Deliberately heap-free: this runs at boot before the kernel heap is
/// initialized. Uses a fixed stack-local buffer plus `black_box` so the
/// compiler can't fold the work away.
#[no_mangle]
pub extern "C" fn abi_check_callee() {
    let mut buf = [0u64; 32];
    for i in 0..buf.len() {
        let x = core::hint::black_box(i as u64);
        buf[i] = x
            .wrapping_mul(0xdead_beef_cafe_babe)
            .wrapping_add(buf[i.wrapping_sub(1) & (buf.len() - 1)]);
    }
    let mut acc: u64 = 0;
    for v in &buf {
        acc = acc.wrapping_add(core::hint::black_box(*v));
    }
    core::hint::black_box(acc);
}

/// Run the SysV ABI preservation check. Panics if a callee-saved register
/// was clobbered — call this once at boot.
pub fn verify_sysv_abi_preservation() {
    let result = unsafe { sysv_abi_preservation_test() };
    if result == 0 {
        klibcluu::info("  SysV ABI preservation check passed (RBX/RBP/R12-R14)");
        return;
    }

    let reg = match result {
        1 => "RBX",
        2 => "RBP",
        3 => "R12",
        4 => "R13",
        5 => "R14",
        _ => "unknown",
    };
    panic!(
        "SysV ABI preservation check FAILED: {} was clobbered (code {}). \
         Syscall fast path would corrupt user state — refusing to boot.",
        reg, result
    );
}
