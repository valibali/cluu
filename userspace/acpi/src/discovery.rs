use alloc::vec;
use alloc::vec::Vec;
use libcluu::{Error, Result};

use crate::fadt::Fadt;
use crate::rsdp::Rsdp;
use crate::tables::{SdtHeader, SdtSignature};

const FADT_MAX_SIZE: usize = 272;

pub fn find_fadt_from_phys(space_token: usize, rsdp_phys: u64) -> Result<Fadt> {
    let mut rsdp_buf = [0u8; 36];
    read_phys(space_token, rsdp_phys, &mut rsdp_buf)?;
    let rsdp = Rsdp::from_bytes(&rsdp_buf).ok_or(Error::NotFound)?;
    if !rsdp.validate_checksum(&rsdp_buf) {
        return Err(Error::InvalidArgument);
    }
    find_fadt_from_rsdp(space_token, &rsdp)
}

pub fn find_fadt_from_rsdp(space_token: usize, rsdp: &Rsdp) -> Result<Fadt> {
    if rsdp.is_v2() && rsdp.xsdt_phys != 0 {
        find_fadt_in_xsdt(space_token, rsdp.xsdt_phys)
    } else {
        find_fadt_in_rsdt(space_token, rsdp.rsdt_phys as u64)
    }
}

fn find_fadt_in_xsdt(space_token: usize, xsdt_phys: u64) -> Result<Fadt> {
    let mut header_buf = [0u8; 36];
    read_phys(space_token, xsdt_phys, &mut header_buf)?;
    let header = SdtHeader::from_bytes(&header_buf).ok_or(Error::NotFound)?;
    if header.signature != SdtSignature::XSDT {
        return Err(Error::NotFound);
    }
    let total = header.length as usize;
    let mut xsdt_buf = vec![0u8; total];
    read_phys(space_token, xsdt_phys, &mut xsdt_buf)?;
    if !header.validate_checksum(&xsdt_buf) {
        return Err(Error::InvalidArgument);
    }
    let entry_start = 36;
    let entry_size = 8;
    let count = (total - entry_start) / entry_size;
    for i in 0..count {
        let off = entry_start + i * entry_size;
        let entry_phys = u64::from_le_bytes([
            xsdt_buf[off], xsdt_buf[off+1], xsdt_buf[off+2], xsdt_buf[off+3],
            xsdt_buf[off+4], xsdt_buf[off+5], xsdt_buf[off+6], xsdt_buf[off+7],
        ]);
        if entry_phys == 0 { continue; }
        if let Some(fadt) = try_read_fadt(space_token, entry_phys) {
            return Ok(fadt);
        }
    }
    Err(Error::NotFound)
}

fn find_fadt_in_rsdt(space_token: usize, rsdt_phys: u64) -> Result<Fadt> {
    let mut header_buf = [0u8; 36];
    read_phys(space_token, rsdt_phys, &mut header_buf)?;
    let header = SdtHeader::from_bytes(&header_buf).ok_or(Error::NotFound)?;
    let _ = libcluu::debug_print(&alloc::format!(
        "acpi: RSDT sig={} len={}", header.signature.as_str(), header.length
    ));
    if header.signature != SdtSignature::RSDT {
        return Err(Error::NotFound);
    }
    let total = header.length as usize;
    let mut rsdt_buf = vec![0u8; total];
    read_phys(space_token, rsdt_phys, &mut rsdt_buf)?;
    if !header.validate_checksum(&rsdt_buf) {
        let _ = libcluu::debug_print("acpi: RSDT checksum failed");
        return Err(Error::InvalidArgument);
    }
    let entry_start = 36;
    let entry_size = 4;
    let count = (total - entry_start) / entry_size;
    let _ = libcluu::debug_print(&alloc::format!("acpi: RSDT {} entries", count));
    for i in 0..count {
        let off = entry_start + i * entry_size;
        let entry_phys = u32::from_le_bytes([
            rsdt_buf[off], rsdt_buf[off+1], rsdt_buf[off+2], rsdt_buf[off+3],
        ]) as u64;
        if entry_phys == 0 { continue; }
        let _ = libcluu::debug_print(&alloc::format!(
            "acpi: RSDT[{}] phys=0x{:x}", i, entry_phys
        ));
        if let Some(fadt) = try_read_fadt(space_token, entry_phys) {
            return Ok(fadt);
        }
    }
    let _ = libcluu::debug_print("acpi: no FADT in RSDT");
    Err(Error::NotFound)
}

fn try_read_fadt(space_token: usize, fadt_phys: u64) -> Option<Fadt> {
    let mut header_buf = [0u8; 36];
    if read_phys(space_token, fadt_phys, &mut header_buf).is_err() {
        return None;
    }
    let header = SdtHeader::from_bytes(&header_buf)?;
    if header.signature != SdtSignature::FADT {
        let _ = libcluu::debug_print(&alloc::format!(
            "acpi: SDT at 0x{:x} sig={} len={}", fadt_phys, header.signature.as_str(), header.length
        ));
        return None;
    }
    let len = (header.length as usize).min(FADT_MAX_SIZE);
    let mut buf = vec![0u8; len];
    if read_phys(space_token, fadt_phys, &mut buf).is_err() {
        return None;
    }
    Fadt::from_bytes(&buf)
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
