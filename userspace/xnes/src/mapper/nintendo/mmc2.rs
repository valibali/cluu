use crate::mapper::MapperImpl;
use alloc::vec::Vec;

/// MMC2 (Mapper 9) — used by Punch-Out!!
///
/// PRG:
///   $8000-$9FFF: 8KB switchable (register $8000, bits 0-3)
///   $A000-$FFFF: Fixed to last 24KB of PRG ROM (3 x 8KB banks)
///
/// CHR:
///   Two 4KB banks with latching mechanism.
///   $A000/$A001: CHR banks for PPU $0000-$0FFF (latch 0/1)
///   $B000/$B001: CHR banks for PPU $1000-$1FFF (latch 0/1)
///   Latch 0 toggled by PPU reads from $0FD8-$0FDF (0) or $0FE8-$0FEF (1)
///   Latch 1 toggled by PPU reads from $1FD8-$1FDF (0) or $1FE8-$1FEF (1)
pub struct Mmc2 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    mirror: u8,
    has_chr_ram: bool,

    // $8000: PRG bank select (8KB at $8000-$9FFF)
    prg_bank: u8,

    // CHR banks: [latch0, latch1] for each 4KB region
    chr_bank_0: [u8; 2], // $A000/$A001: banks for PPU $0000-$0FFF
    chr_bank_1: [u8; 2], // $B000/$B001: banks for PPU $1000-$1FFF

    // Current latches (0 or 1)
    latch_0: u8,
    latch_1: u8,
}

impl Mmc2 {
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
            latch_0: 1,
            latch_1: 1,
        }
    }

    fn prg_8k_count(&self) -> u8 {
        (self.prg.len() / 0x2000).max(1) as u8
    }
}

impl MapperImpl for Mmc2 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let count = self.prg_8k_count() as usize;
        match addr {
            0x8000..=0x9FFF => {
                let bank = (self.prg_bank as usize) % count;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xA000..=0xFFFF => {
                // Fixed: last 24KB = 3 x 8KB banks at count-3, count-2, count-1
                let fixed_start = count.saturating_sub(3);
                let bank_idx = ((addr - 0xA000) / 0x2000) as usize;
                let bank = (fixed_start + bank_idx.min(2)) % count;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x8000..=0x9FFF => {
                // Even/odd address: A0=0 stores low nibble, A0=1 stores high nibble
                if addr & 1 == 0 {
                    self.prg_bank = (self.prg_bank & 0xF0) | (val & 0x0F);
                } else {
                    self.prg_bank = (self.prg_bank & 0x0F) | ((val & 0x0F) << 4);
                }
            }
            0xA000..=0xAFFF => {
                // CHR bank for PPU $0000-$0FFF (A0=0=latch0, A0=1=latch1)
                self.chr_bank_0[(addr & 1) as usize] = val;
            }
            0xB000..=0xBFFF => {
                // CHR bank for PPU $1000-$1FFF (A0=0=latch0, A0=1=latch1)
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

        // Save old latch values — the current read uses the OLD latch;
        // the change takes effect on the NEXT read (real MMC2/MMC4 behavior).
        let old_latch_0 = self.latch_0;
        let old_latch_1 = self.latch_1;

        // CHR latching:
        // PPU $0FD8-$0FDF → latch_0 = 0
        // PPU $0FE8-$0FEF → latch_0 = 1
        // PPU $1FD8-$1FDF → latch_1 = 0
        // PPU $1FE8-$1FEF → latch_1 = 1
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
