use crate::mapper::MapperImpl;
use alloc::vec::Vec;

pub struct Mmc3 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    // Bank select
    bank_select: u8, // $8000
    // PRG banks
    prg_banks: [u8; 4], // 0=R2, 1=R3 (2KB each), 2=R4 (8KB), 3=R5 (8KB)
    // CHR banks
    chr_banks: [u8; 8], // 2KB each
    // IRQ
    irq_latch: u8,
    irq_counter: u8,
    irq_enabled: bool,
    irq_reload: bool,
    irq_flag: bool,
    // PRG RAM
    prg_ram: [u8; 0x2000],
    prg_ram_enable: bool,
    prg_ram_write: bool,
    has_chr_ram: bool,
}

impl Mmc3 {
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
            bank_select: 0,
            prg_banks: [0; 4],
            chr_banks: [0; 8],
            irq_latch: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_reload: false,
            irq_flag: false,
            prg_ram: [0; 0x2000],
            prg_ram_enable: false,
            prg_ram_write: false,
            has_chr_ram: chr_ram,
        }
    }

    fn prg_bank_count(&self) -> u8 {
        (self.prg.len() / 0x2000) as u8
    }

    fn chr_bank_count(&self) -> u8 {
        if self.chr.is_empty() {
            return 1;
        }
        (self.chr.len() / 0x200) as u8
    }

    fn chr_addr(&self, bank: u8, off: u16) -> u16 {
        let b = (bank as usize) * 0x200 + (off as usize) % 0x200;
        if self.chr.is_empty() {
            return 0;
        }
        (b % self.chr.len()) as u16
    }
}

impl MapperImpl for Mmc3 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                // PRG RAM returns data only when enabled (bit 7 of $A001)
                if self.prg_ram_enable {
                    self.prg_ram[(addr & 0x1FFF) as usize]
                } else {
                    0 // open bus when disabled
                }
            }
            0x8000..=0x9FFF => {
                // 8KB switchable or fixed (controlled by bank_select bit 6)
                // Real MMC3: bit 6=0 => $8000 = R6 (switchable), $C000 = fixed-2
                //            bit 6=1 => $8000 = fixed-2, $C000 = R6 (switchable)
                let bank = if self.bank_select & 0x40 != 0 {
                    // $8000 fixed to second-to-last bank (swapped mode)
                    (self.prg_bank_count() - 2) as usize
                } else {
                    // $8000 = R6 (default: switchable)
                    self.prg_banks[2] as usize
                };
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xA000..=0xBFFF => {
                // Always R7 (switchable)
                let bank = self.prg_banks[3] as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xC000..=0xDFFF => {
                // 8KB switchable or fixed
                if self.bank_select & 0x40 != 0 {
                    // $C000 = R6 (swapped mode: switchable)
                    let bank = self.prg_banks[2] as usize;
                    let off = (addr & 0x1FFF) as usize;
                    self.prg[(bank * 0x2000 + off) % self.prg.len()]
                } else {
                    // $C000 fixed to second-to-last bank (default mode)
                    let bank = (self.prg_bank_count() - 2) as usize;
                    let off = (addr & 0x1FFF) as usize;
                    self.prg[(bank * 0x2000 + off) % self.prg.len()]
                }
            }
            0xE000..=0xFFFF => {
                // Fixed to last bank
                let bank = (self.prg_bank_count() - 1) as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            _ => 0,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                // Write allowed when PRG RAM enabled AND bit 6 of $A001 is 0
                // (bit 6 = 1 means write-protected)
                if self.prg_ram_enable && !self.prg_ram_write {
                    self.prg_ram[(addr & 0x1FFF) as usize] = val;
                }
            }
            0x8000..=0x9FFF => {
                if addr & 1 == 0 {
                    // $8000: Bank select
                    self.bank_select = val & 0xE7; // bits 5,6 used; mask out bit 2
                // Update PRG mode and CHR mode from bit 6 and bit 7
                } else {
                    // $8001: Bank data
                    let mode = self.bank_select & 0x07;
                    let bank = val;
                    match mode {
                        0 => self.chr_banks[0] = bank & 0xFE,
                        1 => self.chr_banks[1] = bank & 0xFE,
                        2 => self.chr_banks[2] = bank,
                        3 => self.chr_banks[3] = bank,
                        4 => self.chr_banks[4] = bank,
                        5 => self.chr_banks[5] = bank,
                        6 => {
                            // R6: switchable PRG bank slot (at $8000 or $C000 depending on mode)
                            self.prg_banks[2] = bank & 0x3F;
                        }
                        7 => {
                            self.prg_banks[3] = bank & 0x3F;
                        }
                        _ => {}
                    }
                }
            }
            0xA000..=0xBFFF => {
                if addr & 1 == 0 {
                    // $A000: Mirroring
                    // MMC3: bit 0 = 0 → vertical, bit 0 = 1 → horizontal
                    // nt_index: mirror 0 = horizontal, mirror 1 = vertical
                    self.mirror = u8::from(val & 1 == 0); // invert to match nt_index
                } else {
                    // $A001: PRG RAM control
                    //   bit 7: PRG RAM enable (1 = enabled)
                    //   bit 6: PRG RAM write protect (1 = protected, writes blocked)
                    self.prg_ram_enable = val & 0x80 != 0;
                    self.prg_ram_write = val & 0x40 != 0;
                }
            }
            0xC000..=0xDFFF => {
                if addr & 1 == 0 {
                    // $C000: IRQ latch
                    self.irq_latch = val;
                } else {
                    // $C001: IRQ reload — reloads counter from latch IMMEDIATELY
                    // (real MMC3 behavior: the counter is updated right away,
                    // not deferred to the next A12 edge).
                    self.irq_counter = self.irq_latch;
                    self.irq_reload = false;
                }
            }
            0xE000..=0xFFFF => {
                if addr & 1 == 0 {
                    // $E000: IRQ disable + acknowledge (clears flag, disables IRQ)
                    self.irq_enabled = false;
                    self.irq_flag = false;
                } else {
                    // $E001: IRQ enable (reload happens on next A12 edge)
                    self.irq_enabled = true;
                }
            }
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            return 0;
        }
        if self.has_chr_ram {
            return if a < 0x2000 { self.chr[a as usize] } else { 0 };
        }
        if self.chr.is_empty() {
            return 0;
        }

        // Determine which 1KB CHR bank is active for this address
        // MMC3 maps PPU $0000-$1FFF to CHR banks in groups:
        // $0000-$07FF: chr_banks[0] * 0x400  (2KB, but actually bank 0 is even, bank 1 is bank0+1)
        // $0800-$0FFF: chr_banks[2] * 0x400  (1KB)
        // $1000-$13FF: chr_banks[4] * 0x400  (1KB)
        // $1400-$17FF: chr_banks[5] * 0x400  (1KB)
        // $1800-$1BFF: chr_banks[6] * 0x400  (1KB)
        // $1C00-$1FFF: chr_banks[7] * 0x400  (1KB)
        //
        // Actually MMC3 CHR banking:
        // 2KB banks: chr_banks[0] (even/odd pair for $0000/$0800)
        // 1KB banks: chr_banks[2]..chr_banks[7] for $1000..$1FFF
        // If bit 7 of bank_select is set, swap 2KB and 1KB groups

        let swap = self.bank_select & 0x80 != 0;
        let ch_len = self.chr.len();

        let (bank_base, bank_size) = if a < 0x1000 {
            if swap {
                // swapped: $0000-$0FFF uses R2-R5 (4x1KB banks, modes 2-5)
                let idx = if a < 0x0400 {
                    2
                } else if a < 0x0800 {
                    3
                } else if a < 0x0C00 {
                    4
                } else {
                    5
                };
                (self.chr_banks[idx] as usize * 0x400, 0x400)
            } else {
                // $0000-$07FF: chr_banks[0] (2KB select)
                // $0800-$0FFF: chr_banks[1] (2KB select)
                let b = if a < 0x0800 {
                    self.chr_banks[0] as usize & 0xFE
                } else {
                    self.chr_banks[1] as usize & 0xFE
                };
                let sub = usize::from((a & 0x400) != 0);
                ((b | sub) * 0x400, 0x400)
            }
        } else {
            // $1000-$1FFF
            if swap {
                // swapped: $1000-$1FFF uses R0/R1 (2KB pair)
                let b = if a < 0x1800 {
                    self.chr_banks[0] as usize & 0xFE
                } else {
                    self.chr_banks[1] as usize & 0xFE
                };
                let sub = usize::from((a & 0x400) != 0);
                ((b | sub) * 0x400, 0x400)
            } else {
                let idx = match (a >> 10) & 3 {
                    0 => 2,
                    1 => 3,
                    2 => 4,
                    _ => 5,
                };
                (self.chr_banks[idx] as usize * 0x400, 0x400)
            }
        };

        let chr_idx = (bank_base + (a as usize & (bank_size - 1))) % ch_len;
        self.chr[chr_idx]
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
        self.irq_flag
    }

    fn ack_irq(&mut self) {
        self.irq_flag = false;
    }

    fn clock_scanline(&mut self) {
        // Real MMC3 behavior (from SJNES reference):
        //   - If counter is 0 or reload is pending: reload from latch
        //   - Otherwise: decrement counter
        //   - If counter (new value) is 0 and IRQ enabled: fire IRQ
        // The counter auto-reloads when reaching 0, so IRQ fires
        // repeatedly every N A12 edges until acknowledged.
        if self.irq_counter == 0 || self.irq_reload {
            self.irq_counter = self.irq_latch;
            self.irq_reload = false;
        } else {
            self.irq_counter -= 1;
        }
        if self.irq_counter == 0 && self.irq_enabled {
            self.irq_flag = true;
        }
    }

    fn has_chr_ram(&self) -> bool {
        self.has_chr_ram
    }
}
