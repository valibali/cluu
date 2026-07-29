use super::super::MapperImpl;
use alloc::vec::Vec;

/// Namco 163 (mapper 19 / 210)
///
/// Multi-game board with PRG/CHR banking, scanline IRQ, and audio.
///
/// PRG: 8 KB banks
///   - $8000-$9FFF: switchable (internal register 0x08)
///   - $A000-$BFFF: switchable (internal register 0x09)
///   - $C000-$DFFF: switchable (internal register 0x0A)
///   - $E000-$FFFF: fixed to last bank (or internal register 0x0B)
///
/// CHR: 1 KB banks (8 slots), controlled by internal registers 0x00-0x07
///
/// Registers accessed via two ports at $4800 and $4801:
///   - $4800 write: set internal address index
///   - $4801 write: write data to internal register at current index
///   - $4801 read:  read data from internal register at current index
///     (address auto-increments after access? some variants do)
///
/// Internal register map (128 x 8-bit):
///   - 0x00-0x07: CHR bank selects (8 x 1 KB banks)
///   - 0x08-0x0B: PRG bank selects (4 x 8 KB banks, typically 0x08=$8000 through 0x0B=$E000)
///   - 0x0C-0x0F: Extra PRG banks / sound control
///   - 0x10-0x7B: Audio (8 channels, 14 bytes each) - SKIPPED
///   - 0x7C: IRQ counter low
///   - 0x7D: IRQ counter high
///   - 0x7E: IRQ enable
///   - 0x7F: IRQ disable / acknowledge
///
/// Audio is not implemented here.
pub struct Namco163 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// 128 internal 8-bit registers (only 0x00-0x0F and 0x7C-0x7F actively used here)
    internal_regs: [u8; 128],
    /// Current register index selected by $4800
    reg_index: u8,
    /// IRQ state
    irq_counter: u16,
    irq_enabled: bool,
    irq_pending: bool,
    /// PRG RAM (8 KB battery-backed)
    prg_ram: [u8; 0x2000],
    /// Previous A12 state for edge detection
    prev_a12: bool,
}

impl Namco163 {
    pub fn new(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        Self {
            prg: prg.to_vec(),
            chr: if chr_ram {
                alloc::vec![0u8; 0x2000]
            } else {
                chr.to_vec()
            },
            chr_ram,
            mirror,
            internal_regs: [0; 128],
            reg_index: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_pending: false,
            prg_ram: [0; 0x2000],
            prev_a12: false,
        }
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg.len() / 0x2000).max(1)
    }

    fn chr_bank_count(&self) -> usize {
        if self.chr.is_empty() {
            1
        } else {
            (self.chr.len() / 0x0400).max(1)
        }
    }

    fn read_prg(&self, bank: usize, offset: usize) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let banks = self.prg_bank_count();
        let idx = (bank % banks) * 0x2000 + (offset % 0x2000);
        self.prg[idx % self.prg.len()]
    }

    /// Get PRG bank index for a given CPU address region from internal registers.
    fn prg_bank_for(&self, slot: usize) -> usize {
        let reg_val = self.internal_regs[0x08 + slot.min(3)] as usize;
        // Some games use more bits for larger PRG, clamp to bank count
        reg_val % self.prg_bank_count()
    }

    /// Clock the scanline IRQ counter.
    fn clock_irq(&mut self) {
        self.irq_counter = self.irq_counter.wrapping_sub(1);
        if self.irq_counter == 0xFFFF {
            // Wrapped around: reload from regs
            let lo = self.internal_regs[0x7C] as u16;
            let hi = self.internal_regs[0x7D] as u16;
            self.irq_counter = (hi << 8) | lo;
            if self.irq_enabled {
                self.irq_pending = true;
            }
        }
    }

    /// Process a write to the internal register array at $4801.
    fn write_internal_reg(&mut self, index: usize, val: u8) {
        self.internal_regs[index & 0x7F] = val;

        match index & 0x7F {
            0x7C => {
                // IRQ counter low byte
                let hi = self.internal_regs[0x7D] as u16;
                self.irq_counter = (hi << 8) | val as u16;
            }
            0x7D => {
                // IRQ counter high byte
                let lo = self.internal_regs[0x7C] as u16;
                self.irq_counter = (val as u16) << 8 | lo;
            }
            0x7E => {
                // IRQ enable: writing any value enables IRQ
                self.irq_enabled = true;
            }
            0x7F => {
                // IRQ disable / acknowledge
                self.irq_enabled = false;
                self.irq_pending = false;
                // Also reload counter from regs
                let lo = self.internal_regs[0x7C] as u16;
                let hi = self.internal_regs[0x7D] as u16;
                self.irq_counter = (hi << 8) | lo;
            }
            _ => {}
        }
    }
}

impl MapperImpl for Namco163 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x4800 => {
                // Read internal address index (most implementations return open bus or last index)
                self.reg_index
            }
            0x4801 => {
                // Read data from internal register at current index
                let idx = (self.reg_index & 0x7F) as usize;
                self.internal_regs[idx]
            }
            0x5000..=0x5FFF => {
                // Mirrors of $4800-$4801 for multi-access
                // Some games use $5000 for read as well
                0
            }
            0x6000..=0x7FFF => {
                // PRG RAM
                self.prg_ram[(addr & 0x1FFF) as usize]
            }
            0x8000..=0x9FFF => {
                let off = (addr & 0x1FFF) as usize;
                self.read_prg(self.prg_bank_for(0), off)
            }
            0xA000..=0xBFFF => {
                let off = (addr & 0x1FFF) as usize;
                self.read_prg(self.prg_bank_for(1), off)
            }
            0xC000..=0xDFFF => {
                let off = (addr & 0x1FFF) as usize;
                self.read_prg(self.prg_bank_for(2), off)
            }
            0xE000..=0xFFFF => {
                let off = (addr & 0x1FFF) as usize;
                // $E000-$FFFF typically fixed to last bank
                let banks = self.prg_bank_count();
                self.read_prg(banks.saturating_sub(1), off)
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x4800 => {
                // Set internal address index
                self.reg_index = val;
            }
            0x4801 => {
                // Write data to internal register at current index
                self.write_internal_reg(self.reg_index as usize, val);
                // Auto-increment? Some Namco 163 variants do, let's not auto-increment
            }
            0x5000..=0x5FFF => {
                // Audio or IRQ mirror area
                // Some games write audio data here
            }
            0x6000..=0x7FFF => {
                // PRG RAM write
                self.prg_ram[(addr & 0x1FFF) as usize] = val;
            }
            0x8000..=0xFFFF => {
                // Some games write to PRG area for banking as well
                // (in case the internal register writes work differently)
                // The primary mechanism is via $4800-$4801 though.
            }
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            // A12 edge detection for nametable range
            if a < 0x3000 {
                let a12 = (a & 0x1000) != 0;
                if a12 && !self.prev_a12 {
                    self.clock_irq();
                }
                self.prev_a12 = a12;
            }
            return 0;
        }

        if self.chr.is_empty() {
            return 0;
        }

        // A12 edge detection on CHR address
        let a12 = (a & 0x1000) != 0;
        if a12 && !self.prev_a12 {
            self.clock_irq();
        }
        self.prev_a12 = a12;

        let banks = self.chr_bank_count();
        let slot = (a >> 10) as usize; // 0-7
        let bank = (self.internal_regs[slot] as usize) % banks;
        let off = (a & 0x03FF) as usize;
        self.chr[bank * 0x0400 + off]
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_ram {
            let a = addr & 0x1FFF;
            if (a as usize) < self.chr.len() {
                self.chr[a as usize] = val;
            }
        }
    }

    fn mirroring(&self) -> u8 {
        self.mirror
    }

    fn irq_pending(&self) -> bool {
        self.irq_pending
    }

    fn ack_irq(&mut self) {
        self.irq_pending = false;
    }

    fn has_chr_ram(&self) -> bool {
        self.chr_ram
    }
}
