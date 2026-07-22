//! Minimal DSDT/SSDT AML parser for PNP device enumeration (D6.1).
//!
//! Walks AML bytecode looking for `Device()` objects and extracts the
//! static `_HID` and `_CRS` name/value pairs from each device.  Does
//! NOT execute AML methods — only parses the structural opcodes that
//! QEMU's DSDT uses for PNP0303 (keyboard), PNP0F13 (mouse), PNP0C02
//! (motherboard resources), and similar.
//!
//! Supported opcodes (sufficient for QEMU's DSDT):
//! - `NameOp` (0x08) with a 4-byte NameSeg (`_HID`, `_CRS`, etc.)
//! - `DeviceOp` (`0x5B 0x82`) with PkgLength, NameSeg, body
//! - `BufferOp` (0x11) for `_CRS` resource templates
//! - Resource descriptors: `IRQNoFlags` (0x22), `IO` (0x47),
//!   `FixedIO` (0x4B), `EndTag` (0x79)
//! - Data encodings: `Zero`/`One`/`Ones`, `BytePrefix`/`WordPrefix`/
//!   `DWordPrefix`/`QWordPrefix`, `StringPrefix`
//!
//! `_HID` is returned either as the literal PNP string (e.g.
//! `"PNP0303"`) or decoded from a 32-bit EISA ID into the canonical
//! `AAAABBBB` form (three letters + four hex digits).

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use libcluu::debug_print;

/// One ACPI device discovered in the DSDT/SSDT.
#[derive(Debug, Clone, Default)]
pub struct AcpiDevice {
    /// PNP hardware ID (`PNP0303`, `PNP0F13`, EISA-decoded `PNP0C02`, ...).
    /// Empty when the device has no parseable `_HID`.
    pub hid: String,
    /// I/O port base addresses claimed by the device via `_CRS`.
    pub io_ports: Vec<u16>,
    /// First IRQ line claimed via `_CRS` `IRQNoFlags`, if any.
    pub irq: Option<u8>,
}

// AML opcodes
const NAME_OP: u8 = 0x08;
const EXT_OP_PREFIX: u8 = 0x5B;
const DEVICE_OP: u8 = 0x82;
const BUFFER_OP: u8 = 0x11;
const ZERO_OP: u8 = 0x00;
const ONE_OP: u8 = 0x01;
const ONES_OP: u8 = 0xFF;
const BYTE_PREFIX: u8 = 0x0A;
const WORD_PREFIX: u8 = 0x0B;
const DWORD_PREFIX: u8 = 0x0C;
const STRING_PREFIX: u8 = 0x0D;
const QWORD_PREFIX: u8 = 0x0E;

// Resource descriptor tags
const IRQ_NO_FLAGS_TAG: u8 = 0x22;
const IO_TAG: u8 = 0x47;
const FIXED_IO_TAG: u8 = 0x4B;
const END_TAG: u8 = 0x79;

/// Parse a DSDT/SSDT image (full SDT, including the 36-byte header)
/// and return all `Device()` objects that declare a `_HID`.
pub fn parse_devices(sdt: &[u8]) -> Vec<AcpiDevice> {
    if sdt.len() < 36 {
        let _ = debug_print("acpi: DSDT too short");
        return Vec::new();
    }
    let aml = &sdt[36..];
    let mut devices = Vec::new();
    collect_devices(aml, 0, aml.len(), &mut devices);
    devices
}

fn collect_devices(aml: &[u8], start: usize, end: usize, devices: &mut Vec<AcpiDevice>) {
    let mut pos = start;
    while pos < end {
        if aml[pos] == EXT_OP_PREFIX && pos + 1 < end && aml[pos + 1] == DEVICE_OP {
            match parse_device(aml, pos + 2, end) {
                Some((device, body_start, body_end)) => {
                    if !device.hid.is_empty() {
                        devices.push(device);
                    }
                    let name_end = body_start + 4;
                    if name_end < body_end {
                        collect_devices(aml, name_end, body_end, devices);
                    }
                    pos = body_end;
                }
                None => break,
            }
        } else {
            pos += 1;
        }
    }
}

fn parse_device(
    aml: &[u8],
    pos: usize,
    end: usize,
) -> Option<(AcpiDevice, usize, usize)> {
    let (pkg_consumed, pkg_len) = parse_pkg_length(aml, pos, end)?;
    let body_start = pos + pkg_consumed;
    let body_end = (pos + pkg_len).min(end);
    if body_start + 4 > end {
        return None;
    }
    let mut device = AcpiDevice::default();
    let mut p = body_start + 4;
    while p < body_end {
        if aml[p] == NAME_OP && p + 5 <= body_end {
            let name_seg = &aml[p + 1..p + 5];
            let value_start = p + 5;
            match name_seg {
                b"_HID" => {
                    if let Some((hid, next)) = parse_hid(aml, value_start, body_end) {
                        device.hid = hid;
                        p = next;
                        continue;
                    }
                }
                b"_CRS" => {
                    if let Some((ports, irq, next)) = parse_crs(aml, value_start, body_end) {
                        device.io_ports.extend(ports);
                        if device.irq.is_none() {
                            device.irq = irq;
                        }
                        p = next;
                        continue;
                    }
                }
                _ => {
                    if let Some(next) = skip_data_object(aml, value_start, body_end) {
                        p = next;
                        continue;
                    }
                }
            }
        }
        if aml[p] == EXT_OP_PREFIX && p + 1 < body_end && aml[p + 1] == DEVICE_OP {
            if let Some((_, _, nested_end)) = parse_device(aml, p + 2, body_end) {
                p = nested_end;
                continue;
            }
        }
        p += 1;
    }
    Some((device, body_start, body_end))
}

fn parse_pkg_length(aml: &[u8], pos: usize, end: usize) -> Option<(usize, usize)> {
    if pos >= end {
        return None;
    }
    let b0 = aml[pos];
    let bytes_used = ((b0 >> 6) + 1) as usize;
    if pos + bytes_used > end {
        return None;
    }
    let mut len = (b0 & 0x3F) as usize;
    for i in 1..bytes_used {
        len |= (aml[pos + i] as usize) << (6 + 8 * (i - 1));
    }
    Some((bytes_used, len))
}

fn parse_hid(aml: &[u8], pos: usize, end: usize) -> Option<(String, usize)> {
    if pos >= end {
        return None;
    }
    match aml[pos] {
        STRING_PREFIX => {
            let mut p = pos + 1;
            let start = p;
            while p < end && aml[p] != 0 {
                p += 1;
            }
            if p >= end {
                return None;
            }
            let s = core::str::from_utf8(&aml[start..p]).ok()?;
            Some((String::from(s), p + 1))
        }
        DWORD_PREFIX => {
            if pos + 5 > end {
                return None;
            }
            let id = u32::from_be_bytes([
                aml[pos + 1],
                aml[pos + 2],
                aml[pos + 3],
                aml[pos + 4],
            ]);
            Some((eisa_id_to_string(id), pos + 5))
        }
        ZERO_OP => Some((String::new(), pos + 1)),
        _ => None,
    }
}

fn eisa_id_to_string(id: u32) -> String {
    let mut bytes = [0u8; 7];
    bytes[0] = (((id >> 26) & 0x1F) as u8).wrapping_add(b'A' - 1);
    bytes[1] = (((id >> 21) & 0x1F) as u8).wrapping_add(b'A' - 1);
    bytes[2] = (((id >> 16) & 0x1F) as u8).wrapping_add(b'A' - 1);
    let product = (id & 0xFFFF) as u16;
    let hex = b"0123456789ABCDEF";
    bytes[3] = hex[((product >> 12) & 0xF) as usize];
    bytes[4] = hex[((product >> 8) & 0xF) as usize];
    bytes[5] = hex[((product >> 4) & 0xF) as usize];
    bytes[6] = hex[(product & 0xF) as usize];
    core::str::from_utf8(&bytes).map(String::from).unwrap_or_default()
}

fn parse_crs(
    aml: &[u8],
    pos: usize,
    end: usize,
) -> Option<(Vec<u16>, Option<u8>, usize)> {
    if pos >= end || aml[pos] != BUFFER_OP {
        return None;
    }
    let (pkg_consumed, pkg_len) = parse_pkg_length(aml, pos + 1, end)?;
    let body_start = pos + 1 + pkg_consumed;
    let body_end = (pos + 1 + pkg_len).min(end);
    let (_buf_size, size_end) = parse_data_object(aml, body_start, body_end)?;
    let buf_start = size_end;
    let mut ports = Vec::new();
    let mut irq = None;
    let mut p = buf_start;
    while p < body_end {
        let tag = aml[p];
        match tag {
            IO_TAG => {
                if p + 6 > body_end {
                    break;
                }
                let base = u16::from_le_bytes([aml[p + 2], aml[p + 3]]);
                ports.push(base);
                p += 6;
            }
            FIXED_IO_TAG => {
                if p + 4 > body_end {
                    break;
                }
                let base = u16::from_le_bytes([aml[p + 1], aml[p + 2]]);
                ports.push(base);
                p += 4;
            }
            IRQ_NO_FLAGS_TAG => {
                if p + 3 > body_end {
                    break;
                }
                let mask = u16::from_le_bytes([aml[p + 1], aml[p + 2]]);
                if mask != 0 && irq.is_none() {
                    irq = Some(mask.trailing_zeros() as u8);
                }
                p += 3;
            }
            END_TAG => break,
            _ => {
                p += 1;
            }
        }
    }
    Some((ports, irq, body_end))
}

fn parse_data_object(aml: &[u8], pos: usize, end: usize) -> Option<(u64, usize)> {
    if pos >= end {
        return None;
    }
    match aml[pos] {
        ZERO_OP => Some((0, pos + 1)),
        ONE_OP => Some((1, pos + 1)),
        ONES_OP => Some((0xFFFF_FFFF_FFFF_FFFF, pos + 1)),
        BYTE_PREFIX => {
            if pos + 2 > end {
                return None;
            }
            Some((aml[pos + 1] as u64, pos + 2))
        }
        WORD_PREFIX => {
            if pos + 3 > end {
                return None;
            }
            Some((
                u16::from_le_bytes([aml[pos + 1], aml[pos + 2]]) as u64,
                pos + 3,
            ))
        }
        DWORD_PREFIX => {
            if pos + 5 > end {
                return None;
            }
            Some((
                u32::from_le_bytes([
                    aml[pos + 1],
                    aml[pos + 2],
                    aml[pos + 3],
                    aml[pos + 4],
                ]) as u64,
                pos + 5,
            ))
        }
        QWORD_PREFIX => {
            if pos + 9 > end {
                return None;
            }
            Some((
                u64::from_le_bytes([
                    aml[pos + 1],
                    aml[pos + 2],
                    aml[pos + 3],
                    aml[pos + 4],
                    aml[pos + 5],
                    aml[pos + 6],
                    aml[pos + 7],
                    aml[pos + 8],
                ]),
                pos + 9,
            ))
        }
        _ => None,
    }
}

fn skip_data_object(aml: &[u8], pos: usize, end: usize) -> Option<usize> {
    if let Some((_, next)) = parse_data_object(aml, pos, end) {
        return Some(next);
    }
    if pos >= end {
        return None;
    }
    match aml[pos] {
        STRING_PREFIX => {
            let mut p = pos + 1;
            while p < end && aml[p] != 0 {
                p += 1;
            }
            if p >= end {
                None
            } else {
                Some(p + 1)
            }
        }
        BUFFER_OP => {
            let (pkg_consumed, pkg_len) = parse_pkg_length(aml, pos + 1, end)?;
            let body_start = pos + 1 + pkg_consumed;
            let body_end = (pos + 1 + pkg_len).min(end);
            if body_start > end {
                None
            } else {
                let (_size, size_end) = parse_data_object(aml, body_start, body_end)
                    .unwrap_or((0, body_start));
                Some(size_end.max(body_start))
            }
        }
        _ => None,
    }
}
