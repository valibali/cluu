pub const FLAG_CARRY: u8 = 0b0000_0001;
pub const FLAG_ZERO: u8 = 0b0000_0010;
pub const FLAG_INTERRUPT: u8 = 0b0000_0100;
pub const FLAG_DECIMAL: u8 = 0b0000_1000;
pub const FLAG_BREAK: u8 = 0b0001_0000;
pub const FLAG_OVERFLOW: u8 = 0b0100_0000;
pub const FLAG_NEGATIVE: u8 = 0b1000_0000;

/// Interrupt vector addresses (CPU memory map).
pub const NMI_VECTOR: u16 = 0xFFFA;
pub const RESET_VECTOR: u16 = 0xFFFC;
pub const IRQ_VECTOR: u16 = 0xFFFE;

/// RP2A03 CPU register file.
///
/// Layout (repr(C), 8 bytes):
///   0..2  pc  — Program Counter
///   2     a   — Accumulator
///   3     x   — Index Register X
///   4     y   — Index Register Y
///   5     st  — Stack Pointer (S)
///   6     sr  — Status Register (P)
///   7         — (padding for u16 alignment)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpuRp2a03 {
    pub pc: u16,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub st: u8,
    pub sr: u8,
}

impl CpuRp2a03 {
    pub fn new(reset_addr: u16) -> Self {
        Self {
            pc: reset_addr,
            st: 0xFD, // Real NES SP = $FD after reset
            sr: 0x24, // I flag set + bit 5 always 1
            ..Self::default()
        }
    }

    #[inline]
    pub fn pc(&self) -> u16 {
        self.pc
    }

    #[inline]
    pub fn set_pc(&mut self, val: u16) {
        self.pc = val;
    }

    #[inline]
    pub fn advance_pc(&mut self, n: u16) {
        self.pc = self.pc.wrapping_add(n);
    }

    #[inline]
    pub fn a(&self) -> u8 {
        self.a
    }

    #[inline]
    pub fn set_a(&mut self, val: u8) {
        self.a = val;
    }

    #[inline]
    pub fn x(&self) -> u8 {
        self.x
    }

    #[inline]
    pub fn set_x(&mut self, val: u8) {
        self.x = val;
    }

    #[inline]
    pub fn y(&self) -> u8 {
        self.y
    }

    #[inline]
    pub fn set_y(&mut self, val: u8) {
        self.y = val;
    }

    #[inline]
    pub fn st(&self) -> u8 {
        self.st
    }

    #[inline]
    pub fn set_st(&mut self, val: u8) {
        self.st = val;
    }

    #[inline]
    pub fn sr(&self) -> u8 {
        self.sr
    }

    #[inline]
    pub fn set_sr(&mut self, val: u8) {
        self.sr = val;
    }

    #[inline]
    pub fn get_flag(&self, flag: u8) -> bool {
        self.sr & flag != 0
    }

    #[inline]
    pub fn set_flag(&mut self, flag: u8, set: bool) {
        self.sr = (self.sr & !flag) | (flag & (set as u8).wrapping_neg());
    }

    #[inline]
    pub fn set_sign(&mut self, val: u8) {
        self.set_flag(FLAG_NEGATIVE, (val & 0x80) != 0);
    }

    #[inline]
    pub fn set_zero(&mut self, val: u8) {
        self.set_flag(FLAG_ZERO, val == 0);
    }

    #[inline]
    pub fn update_zn_flags(&mut self, val: u8) {
        let z = (val == 0) as u8;
        self.sr = (self.sr & 0x7D) | (z.wrapping_neg() & FLAG_ZERO) | (val & FLAG_NEGATIVE);
    }
}

impl Default for CpuRp2a03 {
    fn default() -> Self {
        Self {
            pc: 0,
            a: 0,
            x: 0,
            y: 0,
            st: 0,
            sr: 0x20, // Bit 5 is always 1 on the 6502
        }
    }
}
