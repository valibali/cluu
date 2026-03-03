//! FPU/SSE State Management
//!
//! This module defines the FPU state buffer used for saving and restoring
//! x87/SSE register state during context switches via FXSAVE/FXRSTOR.
//!
//! Each thread has its own `FpuState` initialized with sane defaults
//! (standard x87 control word and MXCSR mask-all-exceptions value).

/// FPU/SSE state buffer for FXSAVE/FXRSTOR (512 bytes, 16-byte aligned)
///
/// # Layout
///
/// The FXSAVE format stores:
/// - x87 FPU state (FCW, FSW, FTW, FOP, FIP, FDP, ST0-ST7)
/// - SSE state (MXCSR, MXCSR_MASK, XMM0-XMM15)
///
/// # Alignment
///
/// FXSAVE/FXRSTOR require the buffer to be 16-byte aligned.
/// The `repr(C, align(16))` attribute ensures this.
#[repr(C, align(16))]
#[derive(Clone)]
pub struct FpuState {
    pub data: [u8; 512],
}

impl core::fmt::Debug for FpuState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FpuState").finish()
    }
}

impl FpuState {
    /// Create a new FPU state with sane initial values.
    ///
    /// Sets:
    /// - FCW (x87 control word) = 0x037F: all exceptions masked, double precision, round-to-nearest
    /// - MXCSR = 0x1F80: all SSE exceptions masked, round-to-nearest, no flags set
    pub fn new_init() -> Self {
        let mut state = Self { data: [0u8; 512] };
        // FCW at offset 0 (little-endian): 0x037F
        state.data[0] = 0x7F;
        state.data[1] = 0x03;
        // MXCSR at offset 24 (little-endian): 0x1F80
        state.data[24] = 0x80;
        state.data[25] = 0x1F;
        state
    }
}

// Compile-time assertions
const _: () = {
    assert!(core::mem::size_of::<FpuState>() == 512);
    assert!(core::mem::align_of::<FpuState>() == 16);
};
