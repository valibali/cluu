//! Minimal tar parser used during kernel bootstrap.

use alloc::vec::Vec;

/// Aligns a value up to the given power-of-two boundary.
fn align_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

fn parse_octal(field: &[u8]) -> Option<usize> {
    let digits = field
        .iter()
        .take_while(|&&b| b != 0 && b != b' ')
        .cloned()
        .collect::<Vec<u8>>();
    if digits.is_empty() {
        return Some(0);
    }
    core::str::from_utf8(&digits)
        .ok()
        .and_then(|s| usize::from_str_radix(s.trim(), 8).ok())
}

/// Search the archive for a file by its path.
pub fn find_file<'a>(archive: &'a [u8], target: &str) -> Option<&'a [u8]> {
    let mut offset = 0usize;
    while offset + 512 <= archive.len() {
        let header = &archive[offset..offset + 512];
        if header.iter().all(|&b| b == 0) {
            break;
        }

        let name_len = header.iter().take(100).position(|&b| b == 0).unwrap_or(100);
        let name = core::str::from_utf8(&header[..name_len]).ok()?;
        let size = parse_octal(&header[124..136])?;

        let data_offset = offset + 512;
        let data_end = data_offset.checked_add(size)?;
        if data_end > archive.len() {
            return None;
        }

        if name == target {
            return Some(&archive[data_offset..data_end]);
        }

        offset += align_up(512 + size, 512);
    }

    None
}
