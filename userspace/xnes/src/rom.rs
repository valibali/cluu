use core::fmt::{Display, Formatter};

use crate::mapper::Mapper;
use alloc::vec::Vec;

/// Errors that can occur when parsing an iNES ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RomError {
    /// Not an iNES ROM (missing or wrong magic bytes `NES\x1A`).
    BadMagic,
    /// ROM data is too short to contain a valid header.
    TooShort,
}

impl Display for RomError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadMagic => f.write_str("not an iNES ROM (bad magic)"),
            Self::TooShort => f.write_str("ROM data too short for header"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RomError {}

pub struct Rom {
    pub prg: Vec<u8>,
    pub chr: Vec<u8>,
    pub mapper_id: u8,
    pub mirroring: u8,
    pub has_chr_ram: bool,
}

impl Rom {
    /// Parse an iNES ROM from raw bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err(RomError::BadMagic)` if the data doesn't start with the
    /// NES magic bytes (`NES\x1A`), or `Err(RomError::TooShort)` if the data
    /// is shorter than the 16-byte iNES header.
    pub fn new(data: &[u8]) -> Result<Self, RomError> {
        if data.len() < 16 {
            return Err(RomError::TooShort);
        }
        if data[0..4] != [0x4E, 0x45, 0x53, 0x1A] {
            return Err(RomError::BadMagic);
        }

        let flags6 = data[6];
        let flags7 = data[7];

        // Detect iNES 2.0: byte 7 bits 2-3 = `10` binary
        // iNES 2.0 stores mapper high nibble in flags7 bits 3-0 instead of bits 7-4.
        let is_ines2 = (flags7 & 0x0C) == 0x08;
        // Mapper number encoding:
        //   iNES 1.0: upper nibble in flags7 bits 3-0 (bits 7-4 are VS/PC10 flags)
        //   iNES 2.0: upper 2 bits in flags7 bits 1-0 (bits 3-2 are '10' marker)
        //   Lower 4 bits always in flags6 bits 7-4.
        let mapper_id = if is_ines2 {
            ((flags7 & 0x03) << 4) | (flags6 >> 4)
        } else {
            ((flags7 & 0x0F) << 4) | (flags6 >> 4)
        };

        let prg_16kb = data[4] as usize;
        let chr_8kb = data[5] as usize;
        let mirroring = flags6 & 0x01;
        let has_chr_ram = chr_8kb == 0;

        let prg_size = prg_16kb * 0x4000;
        let header_size = if flags6 & 0x04 != 0 { 16 + 512 } else { 16 };

        let data_start = header_size;
        let data_end = data.len();
        let prg_end = (data_start + prg_size).min(data_end);

        let prg = data[data_start..prg_end].to_vec();
        let chr = if has_chr_ram {
            alloc::vec![0u8; 0x2000]
        } else {
            let chr_size = chr_8kb * 0x2000;
            let chr_start = data_start + prg_size;
            let chr_end = (chr_start + chr_size).min(data_end);
            data[chr_start..chr_end].to_vec()
        };

        Ok(Self {
            prg,
            chr,
            mapper_id,
            mirroring,
            has_chr_ram,
        })
    }

    pub fn create_mapper(&self) -> Mapper {
        Mapper::from_ines(
            self.mapper_id,
            self.mirroring,
            &self.prg,
            &self.chr,
            self.has_chr_ram,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_header(prg_banks: u8, chr_banks: u8, flags6: u8, flags7: u8) -> Vec<u8> {
        let mut h = vec![0x4E, 0x45, 0x53, 0x1A, prg_banks, chr_banks, flags6, flags7];
        h.resize(16, 0);
        h
    }

    #[test]
    fn detect_ines2_mapper() {
        // iNES 2.0 header with mapper 4: flags6=0x40, flags7=0x08 (iNES2 marker)
        // iNES 1.0 formula would give mapper 8|4 = 12 (WRONG)
        // iNES 2.0 formula should give mapper 4 (CORRECT)
        let data = fake_header(1, 0, 0x40, 0x08);
        let rom = Rom::new(&data).unwrap();
        assert_eq!(
            rom.mapper_id, 4,
            "iNES 2.0 should decode mapper 4, got {}",
            rom.mapper_id
        );
    }

    #[test]
    fn detect_ines1_mapper() {
        // iNES 1.0 header with mapper 4: flags6=0x40, flags7=0x00
        let data = fake_header(1, 0, 0x40, 0x00);
        let rom = Rom::new(&data).unwrap();
        assert_eq!(
            rom.mapper_id, 4,
            "iNES 1.0 should decode mapper 4, got {}",
            rom.mapper_id
        );
    }

    #[test]
    fn detect_ines1_vs_unisystem() {
        // iNES 1.0 with VS Unisystem flag (flags7 bit 7=1) should NOT affect mapper
        // Mapper = (flags7 & 0x0F) << 4 | (flags6 >> 4) = (0x00) << 4 | 1 = 1
        let data = fake_header(1, 0, 0x10, 0x80);
        let rom = Rom::new(&data).unwrap();
        assert_eq!(
            rom.mapper_id, 1,
            "VS flag should not affect mapper, got {}",
            rom.mapper_id
        );
    }

    #[test]
    fn invalid_header_rejected() {
        assert!(Rom::new(b"").is_err());
        assert!(Rom::new(b"NOPE").is_err());
        assert!(Rom::new(&[0; 16]).is_err());
        let mut valid = vec![0x4E, 0x45, 0x53, 0x1A, 1, 0, 0, 0];
        valid.resize(16, 0);
        valid.extend(&[0xAB; 0x4000]);
        assert!(Rom::new(&valid).is_ok());
    }

    #[test]
    fn nrom_16kb_prg_no_chr() {
        let prg = vec![0xABu8; 0x4000];
        let mut data = fake_header(1, 0, 0x00, 0x00);
        data.extend(&prg);
        let rom = Rom::new(&data).unwrap();
        assert_eq!(rom.mapper_id, 0);
        assert_eq!(rom.mirroring, 0);
        assert!(rom.has_chr_ram);
        assert_eq!(rom.prg.len(), 0x4000);
    }

    #[test]
    fn nrom_16kb_prg_with_chr() {
        let prg = vec![0xCDu8; 0x4000];
        let chr = vec![0xEFu8; 0x2000];
        let mut data = fake_header(1, 1, 0x00, 0x00);
        data.extend(&prg);
        data.extend(&chr);
        let rom = Rom::new(&data).unwrap();
        assert_eq!(rom.mapper_id, 0);
        assert!(!rom.has_chr_ram);
        assert_eq!(rom.chr[0], 0xEF);
        assert_eq!(rom.chr[0x1FFF], 0xEF);
    }

    #[test]
    fn nrom_32kb_prg() {
        let prg = vec![0x42u8; 0x8000];
        let mut data = fake_header(2, 0, 0x00, 0x00);
        data.extend(&prg);
        let rom = Rom::new(&data).unwrap();
        assert_eq!(rom.prg.len(), 0x8000);
        assert!(rom.has_chr_ram);
    }

    #[test]
    fn mapper_detected() {
        let data = fake_header(1, 0, 0x50, 0x10);
        let rom = Rom::new(&data).unwrap();
        assert_eq!(rom.mapper_id, 5);
    }

    #[test]
    fn mirroring_detected() {
        let h = fake_header(1, 0, 0x01, 0x00);
        assert_eq!(Rom::new(&h).unwrap().mirroring, 1);
        let v = fake_header(1, 0, 0x00, 0x00);
        assert_eq!(Rom::new(&v).unwrap().mirroring, 0);
    }

    #[test]
    fn trainer_handled() {
        let prg = vec![0xFFu8; 0x4000];
        let mut data = fake_header(1, 0, 0x04, 0x00);
        data.extend(&[0xAA; 512]);
        data.extend(&prg);
        let rom = Rom::new(&data).unwrap();
        assert_eq!(rom.prg[0], 0xFF);
        assert!(rom.has_chr_ram);
    }

    #[test]
    fn create_mapper_nrom() {
        let prg = vec![0xABu8; 0x4000];
        let mut data = fake_header(1, 0, 0x00, 0x00);
        data.extend(&prg);
        let rom = Rom::new(&data).unwrap();
        let mut mapper = rom.create_mapper();
        assert_eq!(mapper.cpu_read(0x8000), 0xAB);
        assert_eq!(mapper.cpu_read(0xC000), 0xAB);
    }

    #[test]
    fn create_mapper_mmc3() {
        let prg = vec![0xABu8; 0x8000];
        let mut data = fake_header(2, 0, 0x00, 0x00);
        data.extend(&prg);
        let rom = Rom::new(&data).unwrap();
        let mut mapper = rom.create_mapper();
        assert_eq!(mapper.cpu_read(0xE000), 0xAB);
    }

    #[test]
    fn create_mapper_uxrom() {
        let prg = vec![0xAAu8; 0x8000];
        let mut data = fake_header(2, 0, 0x00, 0x00);
        data.extend(&prg);
        let rom = Rom::new(&data).unwrap();
        let mut mapper = rom.create_mapper();
        assert_eq!(mapper.cpu_read(0x8000), 0xAA);
        mapper.cpu_write(0x8000, 1);
        assert_eq!(mapper.cpu_read(0x8000), 0xAA);
    }

    #[test]
    fn mmc3_basic_read_write() {
        // Create a proper mapper 4 (MMC3) ROM with known data
        let mut data = vec![0x4E, 0x45, 0x53, 0x1A];
        data.push(1); // prg_16kb = 1 (16KB)
        data.push(1); // chr_8kb = 1 (8KB)
        data.push(0x40); // flags6: mapper low nibble = 4
        data.push(0x00); // flags7: mapper high nibble = 0, not iNES 2.0
        data.resize(16, 0);
        data.extend(&[0xABu8; 0x4000]); // 16KB PRG
        data.extend(&[0xCDu8; 0x2000]); // 8KB CHR

        let rom = Rom::new(&data).unwrap();
        assert_eq!(
            rom.mapper_id, 4,
            "should detect mapper 4, got {}",
            rom.mapper_id
        );
        assert!(!rom.has_chr_ram);

        let mut mapper = rom.create_mapper();

        // MMC3 default state: PRG bank 0 at $8000 (switchable, but initially 0)
        // With 16KB PRG (2 banks), prg_bank_count = 2, fixed bank = last = 1
        // $8000 = fixed to bank prg_bank_count-2 = 0
        // $E000 = fixed to last bank = 1
        assert_eq!(
            mapper.cpu_read(0x8000),
            0xAB,
            "$8000 should read PRG bank 0"
        );
        assert_eq!(
            mapper.cpu_read(0xE000),
            0xAB,
            "$E000 should read PRG bank 1"
        );

        // CHR reads should return CHR data from bank 0 (all banks start at 0)
        assert_eq!(
            mapper.ppu_read(0x0000),
            0xCD,
            "PPU $0000 should read CHR bank 0"
        );
        assert_eq!(
            mapper.ppu_read(0x1FFF),
            0xCD,
            "PPU $1FFF should read CHR bank 0"
        );
    }

    #[test]
    fn smb3_header_parses_correctly() {
        // Replicate Super Mario Bros 3 header: mapper 4, 256KB PRG, 128KB CHR
        let mut data = vec![0x4E, 0x45, 0x53, 0x1A]; // NES magic
        data.push(0x10); // 16 * 16KB = 256KB PRG
        data.push(0x10); // 16 * 8KB = 128KB CHR
        data.push(0x40); // flags6: mapper low nibble = 4, no trainer
        data.push(0x00); // flags7: no VS, not iNES 2.0
        data.resize(16, 0x00);

        // Fill PRG (256KB) with pattern: low byte of each 8KB bank = bank_number
        for bank in 0..32u8 {
            let mut block = vec![0xFFu8; 0x2000];
            block[0] = bank;
            data.extend(&block);
        }

        // Fill CHR (128KB) with zeros
        data.extend(vec![0x00u8; 0x20000]);

        let rom = Rom::new(&data).unwrap();
        assert_eq!(
            rom.mapper_id, 4,
            "SMB3 should be mapper 4, got {}",
            rom.mapper_id
        );
        assert_eq!(
            rom.prg.len(),
            0x40000,
            "PRG should be 256KB, got {}KB",
            rom.prg.len() / 1024
        );
        assert_eq!(
            rom.chr.len(),
            0x20000,
            "CHR should be 128KB, got {}KB",
            rom.chr.len() / 1024
        );
        assert!(!rom.has_chr_ram, "SMB3 uses CHR ROM");

        // Verify the mapper reads the correct boot bank
        // MMC3 default mode (bit 6=0): $8000 = R6 (switchable, initially bank 0)
        //                              $E000 = fixed to last bank (index 31)
        let mut mapper = rom.create_mapper();
        assert_eq!(
            mapper.cpu_read(0x8000),
            0,
            "$8000 should be PRG bank 0 (R6 init)"
        );
        assert_eq!(mapper.cpu_read(0xE000), 31, "$E000 should be PRG bank 31");
    }

    #[test]
    fn mmc3_prg_bank_switching() {
        let mut data = vec![0x4E, 0x45, 0x53, 0x1A];
        data.push(16); // prg_16kb = 16 (256KB)
        data.push(1); // chr_8kb = 1 (8KB)
        data.push(0x40); // mapper 4
        data.push(0x00);
        data.resize(16, 0);

        // Fill PRG: each 8KB bank has a unique byte value = bank_number
        for bank in 0..32u8 {
            data.extend(std::iter::repeat(bank).take(0x2000));
        }
        data.extend(&[0x00u8; 0x2000]); // 8KB CHR

        let rom = Rom::new(&data).unwrap();
        assert_eq!(rom.mapper_id, 4);
        let mut mapper = rom.create_mapper();

        // Default PRG mode (bit 6=0): $8000 = R6 (switchable, initially bank 0)
        //                             $C000 = fixed to second-to-last (30)
        //                             $E000 = fixed to last bank (31)
        assert_eq!(
            mapper.cpu_read(0x8000),
            0,
            "$8000 should be PRG bank 0 (R6 init)"
        );
        assert_eq!(mapper.cpu_read(0xE000), 31, "$E000 should be PRG bank 31");

        // Write R6=5 via $8001: $8000 should now read bank 5
        mapper.cpu_write(0x8000, 0x06);
        mapper.cpu_write(0x8001, 5);
        assert_eq!(
            mapper.cpu_read(0x8000),
            5,
            "$8000 should be PRG bank 5 after R6 write"
        );
        assert_eq!(
            mapper.cpu_read(0xC000),
            30,
            "$C000 should be PRG bank 30 (fixed-2)"
        );
    }

    #[test]
    #[cfg(feature = "std")]
    fn download_and_parse_nova() {
        let resp = ureq::get(
            "https://github.com/NovaSquirrel/NovaTheSquirrel/releases/download/v1.0.6a/nova.nes",
        )
        .call()
        .unwrap();
        let data = resp.into_body().read_to_vec().unwrap();
        assert!(data.len() > 16);
        let rom = Rom::new(&data).unwrap();
        assert!(rom.prg.len() > 0);
        assert_eq!(rom.mapper_id, 1);
    }
}
