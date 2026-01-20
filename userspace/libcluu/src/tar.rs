//! Helper for parsing simple tar archives in userspace.

use alloc::string::String;
use alloc::vec::Vec;
use core::str;

const BLOCK_SIZE: usize = 512;

/// Tar entry information
pub struct TarEntry {
    pub name: String,
    pub size: usize,
    pub is_dir: bool,
}

fn align_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
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

/// List all entries in a tar archive under a given directory prefix.
///
/// If `dir_prefix` is empty, lists all top-level entries.
/// Returns entries that are direct children of the directory (not nested deeper).
pub fn list_entries(archive: &[u8], dir_prefix: &str) -> Vec<TarEntry> {
    let mut entries = Vec::new();
    let mut seen = alloc::collections::BTreeSet::new();
    let mut offset = 0;

    // Normalize prefix: remove leading "./", ensure no trailing "/"
    let prefix = dir_prefix
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/');

    while offset + BLOCK_SIZE <= archive.len() {
        let header = &archive[offset..offset + BLOCK_SIZE];
        if header.iter().all(|&b| b == 0) {
            break;
        }

        let name_len = header.iter().take(100).position(|&b| b == 0).unwrap_or(100);
        let entry_name = match str::from_utf8(&header[..name_len]) {
            Ok(s) => s,
            Err(_) => {
                offset += BLOCK_SIZE;
                continue;
            }
        };

        let size = parse_octal(&header[124..136]).unwrap_or(0);
        let typeflag = header[156];

        // Normalize entry name
        let normalized = entry_name
            .trim_start_matches("./")
            .trim_start_matches('/')
            .trim_end_matches('/');

        // Check if this entry is under our prefix
        let rel_path = if prefix.is_empty() {
            normalized
        } else if normalized.starts_with(prefix) && normalized.len() > prefix.len() {
            let rest = &normalized[prefix.len()..];
            if rest.starts_with('/') {
                &rest[1..]
            } else {
                offset += align_up(BLOCK_SIZE + size, BLOCK_SIZE);
                continue;
            }
        } else {
            offset += align_up(BLOCK_SIZE + size, BLOCK_SIZE);
            continue;
        };

        // Get just the first component (direct child)
        let child_name = if let Some(slash_pos) = rel_path.find('/') {
            &rel_path[..slash_pos]
        } else {
            rel_path
        };

        if !child_name.is_empty() && !seen.contains(child_name) {
            seen.insert(String::from(child_name));

            // Determine if it's a directory
            let is_dir = typeflag == b'5' || rel_path.contains('/');

            entries.push(TarEntry {
                name: String::from(child_name),
                size,
                is_dir,
            });
        }

        offset += align_up(BLOCK_SIZE + size, BLOCK_SIZE);
    }

    entries
}
