use crate::mapper::MapperImpl;
use alloc::vec::Vec;

/// MMC4 (Mapper 10) — used by Fire Emblem (Japan)
///
/// PRG:
///   $8000-$BFFF: 16KB switchable (register $8000, bits 0-3)
///   $C000-$FFFF: Fixed to last 16KB bank
///
/// CHR:
///   Two 4KB banks with latching (same latch mechanism as MMC2):
///   $A000/$A001: CHR banks for PPU $0000-$0FFF (latch 0/1)
///   $B000/$B001: CHR banks for PPU $1000-$1FFF (latch 0/1)
///   Latch 0 toggled by PPU reads from $0FD8-$0FDF (0) or $0FE8-$0FEF (1)
///   Latch 1 toggled by PPU reads from $1FD8-$1FDF (0) or $1FE8-$1FEF (1)
pub struct Mmc4 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    mirror: u8,
    has_chr_ram: bool,

    // $8000: PRG bank select (16KB at $8000-$BFFF)
    prg_bank: u8,

    // CHR banks: [latch0, latch1] for each 4KB region
    chr_bank_0: [u8; 2], // $A000/$A001: banks for PPU $0000-$0FFF
    chr_bank_1: [u8; 2], // $B000/$B001: banks for PPU $1000-$1FFF

    // Current latches (0 or 1)
    latch_0: u8,
    latch_1: u8,
}

impl Mmc4 {
    pub fn new(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        Self {
            prg: prg.to_vec(),
            chr: if chr_ram {
                alloc::vec![0u8; 0x2000]
            } else {
                chr.to_vec()
            },
            mirror,
            has_chr_ram: chr_ram,
            prg_bank: 0,
            chr_bank_0: [0, 0],
            chr_bank_1: [0, 0],
            latch_0: 0,
            latch_1: 0,
        }
    }

    fn prg_16k_count(&self) -> u8 {
        (self.prg.len() / 0x4000).max(1) as u8
    }
}

impl MapperImpl for Mmc4 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let count = self.prg_16k_count() as usize;
        match addr {
            0x8000..=0xBFFF => {
                // Switchable 16KB bank
                let bank = (self.prg_bank as usize) % count;
                let off = (addr & 0x3FFF) as usize;
                self.prg[(bank * 0x4000 + off) % self.prg.len()]
            }
            0xC000..=0xFFFF => {
                // Fixed to last 16KB bank
                let bank = count - 1;
                let off = (addr & 0x3FFF) as usize;
                self.prg[(bank * 0x4000 + off) % self.prg.len()]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x8000..=0x9FFF => {
                // MMC4: 4-bit PRG bank register at $8000-$9FFF
                // Both even and odd addresses write the same register
                self.prg_bank = val & 0x0F;
            }
            0xA000..=0xAFFF => {
                self.chr_bank_0[(addr & 1) as usize] = val;
            }
            0xB000..=0xBFFF => {
                self.chr_bank_1[(addr & 1) as usize] = val;
            }
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 || self.chr.is_empty() {
            return 0;
        }
        if self.has_chr_ram {
            return self.chr[a as usize];
        }

        // Save old latch values before updating — the current read uses the
        // OLD latch; the change takes effect on the NEXT read (real NES behavior).
        let old_latch_0 = self.latch_0;
        let old_latch_1 = self.latch_1;

        // MMC2/MMC4 CHR latching: PPU address watches switch which CHR bank
        // is used for subsequent reads.
        match a {
            0x0FD8..=0x0FDF => self.latch_0 = 0,
            0x0FE8..=0x0FEF => self.latch_0 = 1,
            0x1FD8..=0x1FDF => self.latch_1 = 0,
            0x1FE8..=0x1FEF => self.latch_1 = 1,
            _ => {}
        }

        let bank = if a < 0x1000 {
            self.chr_bank_0[old_latch_0 as usize] as usize
        } else {
            self.chr_bank_1[old_latch_1 as usize] as usize
        };

        let idx = (bank * 0x1000 + (a as usize & 0xFFF)) % self.chr.len();
        self.chr[idx]
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.has_chr_ram {
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
        self.has_chr_ram
    }
}
