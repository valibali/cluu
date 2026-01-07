//! Helper for parsing simple tar archives in userspace.

use core::str;

const BLOCK_SIZE: usize = 512;

fn align_up(value: usize, align: usize) -> usize {
    ((value + align - 1) / align) * align
}

fn parse_octal(field: &[u8]) -> Option<usize> {
    let s = str::from_utf8(field)
        .ok()?
        .trim_end_matches(char::from(0))
        .trim();
    if s.is_empty() {
        return Some(0);
    }
    usize::from_str_radix(s, 8).ok()
}

/// Locate a member inside a tar archive.
pub fn find_member<'a>(archive: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let mut offset = 0;

    while offset + BLOCK_SIZE <= archive.len() {
        let header = &archive[offset..offset + BLOCK_SIZE];
        if header.iter().all(|&b| b == 0) {
            break;
        }

        let name_len = header.iter().take(100).position(|&b| b == 0).unwrap_or(100);
        let entry_name = str::from_utf8(&header[..name_len]).ok()?;

        let size = parse_octal(&header[124..136])?;
        let data_offset = offset + BLOCK_SIZE;
        let data_end = data_offset.checked_add(size)?;
        if data_end > archive.len() {
            return None;
        }

        if entry_name == name {
            return Some(&archive[data_offset..data_end]);
        }

        offset += align_up(BLOCK_SIZE + size, BLOCK_SIZE);
    }

    None
}
