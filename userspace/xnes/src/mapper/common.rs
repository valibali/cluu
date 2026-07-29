//! Common helper types and utilities for mapper implementations.
//!
//! These reduce boilerplate for the most common NES mapper patterns:
//! PRG/CHR banking, mirroring control, scanline IRQs, etc.

use alloc::vec::Vec;
use alloc::vec;

// ---------------------------------------------------------------------------
// PRG ROM banking
// ---------------------------------------------------------------------------

/// Manages PRG ROM banking with support for common mapping patterns.
/// Bank size is typically 0x2000 (8 KB) or 0x4000 (16 KB).
pub struct PrgRom {
    data: Vec<u8>,
    bank_size: usize,
    bank_count: usize,
}

impl PrgRom {
    pub fn new(data: &[u8], bank_size: usize) -> Self {
        let bank_count = if data.is_empty() {
            1
        } else {
            data.len() / bank_size
        };
        Self {
            data: if data.is_empty() {
                vec![0; bank_size]
            } else {
                data.to_vec()
            },
            bank_size,
            bank_count,
        }
    }

    /// Number of banks
    pub fn bank_count(&self) -> usize {
        self.bank_count
    }

    /// Total size in bytes
    pub fn total_size(&self) -> usize {
        self.data.len()
    }

    /// Bank size in bytes
    pub fn bank_size(&self) -> usize {
        self.bank_size
    }

    /// Read from PRG ROM at the given address.
    /// `bank` is the selected bank index.
    /// `offset` is the offset within the bank (0..bank_size).
    pub fn read(&self, bank: usize, offset: usize) -> u8 {
        let idx = (bank % self.bank_count) * self.bank_size + (offset % self.bank_size);
        self.data[idx % self.data.len()]
    }

    /// Read directly from a CPU address range by selecting the bank
    /// and using addr bits as offset.
    pub fn read_addr(&self, bank: usize, addr: u16, mask: u16) -> u8 {
        let offset = (addr & mask) as usize;
        self.read(bank, offset)
    }

    /// Get total data as slice
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

// ---------------------------------------------------------------------------
// CHR banking (ROM or RAM)
// ---------------------------------------------------------------------------

/// Manages CHR memory (ROM or RAM) with configurable banking.
/// Bank size is typically 0x0400 (1 KB) for fine-grained MMC3-style banking,
/// or 0x2000 (8 KB) for simple mappers.
pub struct ChrMem {
    data: Vec<u8>,
    is_ram: bool,
    bank_size: usize,
    bank_count: usize,
}

impl ChrMem {
    pub fn new(data: &[u8], is_ram: bool, bank_size: usize) -> Self {
        let data = if is_ram && data.is_empty() {
            vec![0; 0x2000] // Default CHR RAM size
        } else if data.is_empty() {
            vec![0; bank_size] // Fallback
        } else {
            data.to_vec()
        };
        let bank_count = data.len() / bank_size;
        Self {
            data,
            is_ram,
            bank_size,
            bank_count,
        }
    }

    pub fn is_ram(&self) -> bool {
        self.is_ram
    }

    pub fn bank_count(&self) -> usize {
        self.bank_count
    }

    pub fn bank_size(&self) -> usize {
        self.bank_size
    }

    pub fn total_size(&self) -> usize {
        self.data.len()
    }

    pub fn read(&self, bank: usize, offset: usize) -> u8 {
        let bank = bank % self.bank_count;
        let idx = bank * self.bank_size + (offset % self.bank_size);
        self.data[idx % self.data.len()]
    }

    pub fn write(&mut self, bank: usize, offset: usize, val: u8) {
        if self.is_ram {
            let bank = bank % self.bank_count;
            let idx = bank * self.bank_size + (offset % self.bank_size);
            let len = self.data.len();
            self.data[idx % len] = val;
        }
    }
}

// ---------------------------------------------------------------------------
// CHR banking with split banks (for MMC3-style 1 KB / 2 KB banking)
// ---------------------------------------------------------------------------

/// CHR banking where banks are selectable via an array of bank indices.
/// Supports 1 KB and 2 KB bank sizes.
pub struct ChrBanked {
    data: Vec<u8>,
    is_ram: bool,
    banks: Vec<usize>, // bank index for each slot
    bank_size: usize,  // 0x0400 for 1 KB banks
    slot_size: usize,  // how many PPU address space each slot covers
    bank_count: usize,
}

impl ChrBanked {
    pub fn new(
        data: &[u8],
        is_ram: bool,
        num_slots: usize,
        slot_size: usize,
        bank_size: usize,
    ) -> Self {
        let data = if is_ram && data.is_empty() {
            vec![0; 0x2000]
        } else if data.is_empty() {
            vec![0; bank_size * num_slots]
        } else {
            data.to_vec()
        };
        let bank_count = data.len().checked_div(bank_size).unwrap_or(0).max(1);
        let banks = vec![0usize; num_slots];
        Self {
            data,
            is_ram,
            banks,
            bank_size,
            slot_size,
            bank_count,
        }
    }

    pub fn is_ram(&self) -> bool {
        self.is_ram
    }

    pub fn set_bank(&mut self, slot: usize, bank: usize) {
        if slot < self.banks.len() {
            self.banks[slot] = bank;
        }
    }

    pub fn get_bank(&self, slot: usize) -> usize {
        if slot < self.banks.len() {
            self.banks[slot]
        } else {
            0
        }
    }

    pub fn num_slots(&self) -> usize {
        self.banks.len()
    }

    pub fn read(&self, addr: u16) -> u8 {
        let slot = (addr as usize / self.slot_size) % self.banks.len();
        let offset = (addr as usize) % self.slot_size;
        let bank = self.banks[slot] % self.bank_count;
        let idx = bank * self.bank_size + (offset % self.bank_size);
        self.data[idx % self.data.len()]
    }

    pub fn write(&mut self, addr: u16, val: u8) {
        if self.is_ram {
            let slot = (addr as usize / self.slot_size) % self.banks.len();
            let offset = (addr as usize) % self.slot_size;
            let bank = self.banks[slot] % self.bank_count;
            let idx = bank * self.bank_size + (offset % self.bank_size);
            let len = self.data.len();
            self.data[idx % len] = val;
        }
    }
}

// ---------------------------------------------------------------------------
// PRG RAM with battery backup
// ---------------------------------------------------------------------------

/// Simple PRG RAM (typically $6000-$7FFF, 8 KB).
pub struct PrgRam {
    data: [u8; 0x2000],
}

impl PrgRam {
    pub fn new() -> Self {
        Self { data: [0; 0x2000] }
    }

    pub fn read(&self, addr: u16) -> u8 {
        self.data[(addr & 0x1FFF) as usize]
    }

    pub fn write(&mut self, addr: u16, val: u8) {
        self.data[(addr & 0x1FFF) as usize] = val;
    }
}

impl Default for PrgRam {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Mirroring helpers
// ---------------------------------------------------------------------------

/// Mirroring mode constants.
pub mod mirror {
    pub const HORIZONTAL: u8 = 0;
    pub const VERTICAL: u8 = 1;
    pub const FOUR_SCREEN: u8 = 2;
    pub const ONE_SCREEN_A: u8 = 3; // All nametables map to A (lower)
    pub const ONE_SCREEN_B: u8 = 4; // All nametables map to B (upper)
}

/// Convert a 2-bit value to a mirroring mode (for MMC1-style mirroring control).
pub fn mirror_from_bits(bits: u8) -> u8 {
    match bits & 3 {
        0 => mirror::ONE_SCREEN_A,
        1 => mirror::ONE_SCREEN_B,
        2 => mirror::VERTICAL,
        3 => mirror::HORIZONTAL,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// MMC3-style scanline IRQ counter
// ---------------------------------------------------------------------------

/// Implements the MMC3/MMC1-style scanline IRQ counter with A12 edge detection.
pub struct ScanlineIrq {
    pub counter: u16,
    pub reload: u16,
    pub enabled: bool,
    pub pending: bool,
    pub reload_on_ack: bool,
}

impl ScanlineIrq {
    pub fn new() -> Self {
        Self {
            counter: 0,
            reload: 0,
            enabled: false,
            pending: false,
            reload_on_ack: false,
        }
    }

    /// A12 edge detection for MMC3 scanline IRQ.
    /// Returns true if the IRQ counter should be clocked.
    pub fn a12_edge(&mut self, addr: u16, prev_a12: &mut bool) -> bool {
        let a12 = (addr & 0x1000) != 0;
        let rising = a12 && !*prev_a12;
        *prev_a12 = a12;
        rising
    }

    /// Clock the scanline IRQ counter. Returns true if IRQ fired.
    pub fn clock(&mut self) -> bool {
        if self.counter == 0 {
            self.counter = self.reload;
        } else {
            self.counter -= 1;
            if self.counter == 0 && self.enabled {
                self.pending = true;
                return true;
            }
        }
        false
    }

    /// Write to the IRQ reload register.
    pub fn write_reload(&mut self, val: u8) {
        self.reload = val as u16;
    }

    /// Write to the IRQ control register (bit 7 = enable, bit 6 = mode, writing also clears counter).
    pub fn write_control(&mut self, val: u8) {
        self.enabled = (val & 0x80) != 0;
        self.counter = 0;
        self.pending = false;
        // bit 6 typically enables "alternate" mode on some mappers
    }

    /// Acknowledge IRQ (clear pending flag).
    pub fn ack(&mut self) {
        self.pending = false;
    }
}

impl Default for ScanlineIrq {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Simple write latch for $8000-$FFFF register decoding
// ---------------------------------------------------------------------------

/// Tracks the last written register index for mapper register sets.
pub struct RegLatch {
    value: u8,
}

impl RegLatch {
    pub fn new() -> Self {
        Self { value: 0 }
    }

    pub fn write(&mut self, val: u8) {
        self.value = val;
    }

    pub fn read(&self) -> u8 {
        self.value
    }
}

impl Default for RegLatch {
    fn default() -> Self {
        Self::new()
    }
}
