use super::super::MapperImpl;
use alloc::vec::Vec;

/// Sunsoft 3 (mapper 67).
///
/// PRG: 32 KB fixed (no banking), or 8 KB banks depending on configuration.
/// CHR: 2 KB banks (4 slots).
/// Mirroring control via register.
///
/// Registers:
///   $8800-$8FFF: Mirroring (bit 0: 0=horizontal, 1=vertical)
///   $4800-$4FFF or $8800-$8FFF: CHR bank 0 (bits 0-2 selects 2KB bank)
///   $9800-$9FFF or $9800-$9FFF: CHR bank 1
///   $A800-$AFFF: CHR bank 2
///   $B800-$BFFF: CHR bank 3
///   $6800-$6FFF or $E800-$EFFF: PRG bank
pub struct Sunsoft3 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    chr_banks: [u8; 4], // 2KB banks at PPU $0000, $0800, $1000, $1800
    prg_bank: u8,
}

impl Sunsoft3 {
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
            chr_banks: [0; 4],
            prg_bank: 0,
        }
    }
}

impl MapperImpl for Sunsoft3 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        match addr {
            0x8000..=0xFFFF => {
                // PRG: 32KB window at $8000-$FFFF
                // Bank register (3 bits, * 32KB) selects which 32KB block
                let prg_len = self.prg.len();
                let bank_start = (self.prg_bank as usize) * 0x8000;
                let idx = bank_start + (addr as usize & 0x7FFF);
                self.prg[idx % prg_len]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            // PRG bank select
            0x4800..=0x4FFF | 0x6800..=0x6FFF | 0xE800..=0xEFFF => {
                self.prg_bank = val & 0x07;
            }
            // Mirroring control
            0x8800..=0x8FFF => {
                self.mirror = u8::from(val & 1 == 0);
            }
            // CHR bank 0
            0x9800..=0x9FFF => {
                self.chr_banks[0] = val & 0x07;
            }
            // CHR bank 1
            0xA800..=0xAFFF => {
                self.chr_banks[1] = val & 0x07;
            }
            // CHR bank 2
            0xB800..=0xBFFF => {
                self.chr_banks[2] = val & 0x07;
            }
            // CHR bank 3
            0xC800..=0xCFFF | 0x7800..=0x7FFF => {
                self.chr_banks[3] = val & 0x07;
            }
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
        let slot = (a as usize) / 0x0800; // 2KB slot
        let offset = (a as usize) & 0x07FF;
        let bank = (self.chr_banks[slot] as usize) & 0x07;
        let idx = (bank * 0x0800 + offset) % self.chr.len();
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
