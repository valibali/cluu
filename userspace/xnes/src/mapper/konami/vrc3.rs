use alloc::vec::Vec;
use alloc::vec;
use super::super::MapperImpl;

/// Konami VRC3 (iNES mapper 73)
///
/// - PRG: 16 KB switchable bank at `$8000`, 16 KB fixed (last bank) at `$C000`
/// - CHR: fixed (no banking, linear access to CHR ROM / RAM)
/// - No IRQ
/// - No mirroring control (uses cartridge header mirroring)
pub struct Vrc3 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    prg_bank: u8, // 16 KB bank number for $8000
}

impl Vrc3 {
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
            prg_bank: 0,
        }
    }
}

impl MapperImpl for Vrc3 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xBFFF => {
                let off = (addr & 0x3FFF) as usize;
                let bank = self.prg_bank as usize;
                let bank_size = 0x4000;
                if self.prg.is_empty() {
                    return 0;
                }
                let idx = (bank * bank_size + off) % self.prg.len();
                self.prg[idx]
            }
            0xC000..=0xFFFF => {
                let off = (addr & 0x3FFF) as usize;
                if self.prg.is_empty() {
                    return 0;
                }
                let prg_len = self.prg.len();
                let last_bank = (prg_len / 0x4000).saturating_sub(1);
                self.prg[last_bank * 0x4000 + off]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x8000..=0xFFFF = addr {
            self.prg_bank = val & 0x0F;
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            return 0;
        }
        if self.chr.is_empty() {
            return 0;
        }
        self.chr[a as usize % self.chr.len()]
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
