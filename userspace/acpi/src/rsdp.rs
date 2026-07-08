use alloc::format;
use libcluu::{Error, Result};

const RSDP_SIGNATURE: &[u8; 8] = b"RSD PTR ";
const EBDA_MIN: u64 = 0x0009_FC00;
const EBDA_MAX: u64 = 0x0009_FFFF;
const ROM_BIOS_MIN: u64 = 0x000E_0000;
const ROM_BIOS_MAX: u64 = 0x000F_FFFF;
const UEFI_ACPI_MIN: u64 = 0x3F00_0000;
const UEFI_ACPI_MAX: u64 = 0x4000_0000;
const RSDP_ALIGN: usize = 16;

#[derive(Debug, Clone, Copy)]
pub struct Rsdp {
    pub signature: [u8; 8],
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub revision: u8,
    pub rsdt_phys: u32,
    pub length: u32,
    pub xsdt_phys: u64,
    pub extended_checksum: u8,
}

impl Rsdp {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 20 { return None; }
        if &data[0..8] != RSDP_SIGNATURE { return None; }
        let mut oem_id = [0u8; 6];
        oem_id.copy_from_slice(&data[10..16]);
        let revision = data[15];
        let rsdt_phys = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        if revision < 2 {
            return Some(Self {
                signature: *RSDP_SIGNATURE,
                checksum: data[9],
                oem_id,
                revision,
                rsdt_phys,
                length: 20,
                xsdt_phys: 0,
                extended_checksum: 0,
            });
        }
        if data.len() < 36 { return None; }
        let length = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
        let xsdt_phys = u64::from_le_bytes([
            data[24], data[25], data[26], data[27],
            data[28], data[29], data[30], data[31],
        ]);
        Some(Self {
            signature: *RSDP_SIGNATURE,
            checksum: data[9],
            oem_id,
            revision,
            rsdt_phys,
            length,
            xsdt_phys,
            extended_checksum: data[32],
        })
    }

    pub fn is_v2(&self) -> bool { self.revision >= 2 }

    pub fn validate_checksum(&self, data: &[u8]) -> bool {
        let len = if self.is_v2() { self.length as usize } else { 20 };
        if data.len() < len { return false; }
        let sum: u8 = data[..len].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        sum == 0
    }
}

pub fn find_rsdp(space_token: usize) -> Result<Rsdp> {
    let _ = libcluu::debug_print("acpi: scanning for RSDP...");
    let regions: &[(u64, u64)] = &[
        (EBDA_MIN, EBDA_MAX),
        (ROM_BIOS_MIN, ROM_BIOS_MAX),
        (UEFI_ACPI_MIN, UEFI_ACPI_MAX),
    ];
    for &(start, end) in regions {
        let mut page_addr = start & !0xFFF;
        while page_addr + 4096 <= end {
            let mut buf = [0u8; 4096];
            if read_phys(space_token, page_addr, &mut buf).is_ok() {
                let scan_end = 4096 - 36;
                let mut off = 0;
                while off <= scan_end {
                    if &buf[off..off+8] == RSDP_SIGNATURE {
                        let mut full = [0u8; 36];
                        let copy_len = 36.min(4096 - off);
                        full[..copy_len].copy_from_slice(&buf[off..off+copy_len]);
                        if copy_len < 36 {
                            let mut page2 = [0u8; 4096];
                            let _ = read_phys(space_token, page_addr + 4096, &mut page2);
                            full[copy_len..].copy_from_slice(&page2[..36-copy_len]);
                        }
                        if let Some(rsdp) = Rsdp::from_bytes(&full) {
                            let _ = libcluu::debug_print(&format!(
                                "acpi: RSDP found at {:x} revision {}",
                                page_addr + off as u64, rsdp.revision
                            ));
                            return Ok(rsdp);
                        }
                    }
                    off += RSDP_ALIGN;
                }
            }
            page_addr += 4096;
        }
    }
    let _ = libcluu::debug_print("acpi: RSDP not found");
    Err(Error::NotFound)
}

fn read_phys(space_token: usize, phys: u64, buf: &mut [u8]) -> Result<()> {
    use libcluu::syscall::space_map_range;
    let pages = (buf.len() + 0xFFF) / 0x1000;
    let tmp_va = 0x5300_0000usize;
    const MAP_DEVICE: usize = 0x100;
    space_map_range(space_token, tmp_va, phys as usize, MAP_DEVICE | 0x03, pages, 0)?;
    unsafe {
        core::ptr::copy_nonoverlapping(tmp_va as *const u8, buf.as_mut_ptr(), buf.len());
    }
    Ok(())
}
