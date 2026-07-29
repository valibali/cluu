use super::super::MapperImpl;
use alloc::vec::Vec;

/// CNROM with copy protection (mapper 185).
///
/// Like CNROM but CHR banking writes must match a specific address pattern:
/// - Writes to $8000-$FFFF: bits 0-3 select CHR bank
/// - Protection: the write address must have bit 2 and bit 4 in a specific
///   relationship for the write to take effect (varies by game).
/// - Specifically, many protected games require that bits 2 and 4 of the
///   write address are equal (both 0 or both 1) for the bank change to occur.
/// - PRG: fixed 32 KB (or mirror of 16 KB), like CNROM.
pub struct CnromProtect {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    chr_bank: u8,
}

impl CnromProtect {
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
            chr_bank: 0,
        }
    }

    /// Check if the write address passes the copy protection check.
    /// Many protected games check that bits 2 and 4 of the address match.
    fn protection_check(addr: u16) -> bool {
        let bit2 = (addr >> 2) & 1;
        let bit4 = (addr >> 4) & 1;
        bit2 == bit4
    }
}

impl MapperImpl for CnromProtect {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => {
                if self.prg.is_empty() {
                    return 0;
                }
                self.prg[(addr & 0x7FFF) as usize % self.prg.len()]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x8000..=0xFFFF = addr {
            // Copy protection: only apply bank change if address passes check
            if Self::protection_check(addr) {
                self.chr_bank = val & 0x0F;
            }
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a < 0x2000 {
            if self.chr.is_empty() {
                return 0;
            }
            let bank_size = 0x2000;
            let banks = (self.chr.len() / bank_size).max(1);
            let bank = (self.chr_bank as usize) % banks;
            self.chr[(bank * bank_size + a as usize) % self.chr.len()]
        } else {
            0
        }
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
