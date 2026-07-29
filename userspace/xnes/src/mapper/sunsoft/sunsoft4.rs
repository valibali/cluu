use super::super::MapperImpl;
use alloc::vec::Vec;

/// Sunsoft 4 (mapper 68).
///
/// PRG: 8 KB banks.
/// CHR: 1 KB banks.
/// Mirroring control.
/// No IRQ.
///
/// Registers:
///   $8000 (byte): PRG bank 0 ($8000)
///   $9000 (byte): PRG bank 1 ($A000)
///   $A000 (byte): PRG bank 2 ($C000)
///   $B000 (byte): PRG bank 3 ($E000)
///   $C000 (byte): Mirroring (bit 0: 0=horizontal, 1=vertical)
///   $D000 (byte): CHR bank 0 ($0000)
///   $D800 (byte): CHR bank 1 ($0400)
///   $E000 (byte): CHR bank 2 ($0800)
///   $E800 (byte): CHR bank 3 ($0C00)
///   $F000 (byte): CHR bank 4 ($1000)
///   $F800 (byte): CHR bank 5 ($1400)
///   $6000 (byte): CHR bank 6 ($1800)
///   $7000 (byte): CHR bank 7 ($1C00)
pub struct Sunsoft4 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    prg_banks: [u8; 4],
    chr_banks: [u8; 8],
}

impl Sunsoft4 {
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
            prg_banks: [0; 4],
            chr_banks: [0; 8],
        }
    }
}

impl MapperImpl for Sunsoft4 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let prg_len = self.prg.len();
        let prg8 = (prg_len / 0x2000).max(1);
        match addr {
            0x8000..=0x9FFF => {
                let bank = (self.prg_banks[0] as usize) % prg8;
                let idx = bank * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            0xA000..=0xBFFF => {
                let bank = (self.prg_banks[1] as usize) % prg8;
                let idx = bank * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            0xC000..=0xDFFF => {
                let bank = (self.prg_banks[2] as usize) % prg8;
                let idx = bank * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            0xE000..=0xFFFF => {
                let bank = (self.prg_banks[3] as usize) % prg8;
                let idx = bank * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x8000..=0x8FFF => self.prg_banks[0] = val,
            0x9000..=0x9FFF => self.prg_banks[1] = val,
            0xA000..=0xAFFF => self.prg_banks[2] = val,
            0xB000..=0xBFFF => self.prg_banks[3] = val,
            0xC000..=0xCFFF => {
                // Mirroring control
                self.mirror = u8::from(val & 1 == 0);
            }
            0xD000..=0xD7FF => self.chr_banks[0] = val,
            0xD800..=0xDFFF => self.chr_banks[1] = val,
            0xE000..=0xE7FF => self.chr_banks[2] = val,
            0xE800..=0xEFFF => self.chr_banks[3] = val,
            0xF000..=0xF7FF => self.chr_banks[4] = val,
            0xF800..=0xFFFF => self.chr_banks[5] = val,
            0x6000..=0x6FFF => self.chr_banks[6] = val,
            0x7000..=0x7FFF => self.chr_banks[7] = val,
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            return 0;
        }
        if self.chr_ram {
            return self.chr[a as usize];
        }
        if self.chr.is_empty() {
            return 0;
        }
        let slot = (a as usize) / 0x0400; // 1KB slot
        let offset = (a as usize) & 0x03FF;
        let bank = self.chr_banks[slot] as usize;
        let idx = (bank * 0x0400 + offset) % self.chr.len();
        self.chr[idx]
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_ram {
            let a = addr & 0x1FFF;
            self.chr[a as usize] = val;
        }
    }

    fn mirroring(&self) -> u8 {
        self.mirror
    }
    fn irq_pending(&self) -> bool {
        false
    }
    fn ack_irq(&mut self) {}
    fn has_chr_ram(&self) -> bool {
        self.chr_ram
    }
}
