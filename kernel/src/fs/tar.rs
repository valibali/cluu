/*
 * USTAR TAR Archive Reader
 *
 * This module implements a reader for USTAR format TAR archives.
 * BOOTBOOT provides the initrd as an uncompressed TAR archive.
 *
 * USTAR Format:
 * - 512-byte headers for each file
 * - File data padded to 512-byte boundaries
 * - Null blocks (1024 bytes of zeros) mark end of archive
 *
 * Header format (USTAR):
 * - Offset 0: filename (100 bytes)
 * - Offset 100: file mode (8 bytes, octal)
 * - Offset 108: owner UID (8 bytes, octal)
 * - Offset 116: group GID (8 bytes, octal)
 * - Offset 124: file size (12 bytes, octal)
 * - Offset 136: modification time (12 bytes, octal)
 * - Offset 148: checksum (8 bytes, octal)
 * - Offset 156: type flag (1 byte)
 * - Offset 157: linked file name (100 bytes)
 * - Offset 257: USTAR indicator "ustar\0" (6 bytes)
 * - Offset 263: USTAR version "00" (2 bytes)
 */

use core::str;

/// TAR header (512 bytes)
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct TarHeader {
    name: [u8; 100],
    mode: [u8; 8],
    uid: [u8; 8],
    gid: [u8; 8],
    size: [u8; 12],
    mtime: [u8; 12],
    checksum: [u8; 8],
    typeflag: u8,
    linkname: [u8; 100],
    magic: [u8; 6],     // "ustar\0"
    version: [u8; 2],   // "00"
    uname: [u8; 32],
    gname: [u8; 32],
    devmajor: [u8; 8],
    devminor: [u8; 8],
    prefix: [u8; 155],
    _padding: [u8; 12],
}

/// TAR file entry
#[derive(Debug, Clone, Copy)]
pub struct TarEntry {
    /// File name (null-terminated)
    pub name: [u8; 256],
    /// File size in bytes
    pub size: usize,
    /// Offset of file data in the archive
    pub data_offset: usize,
    /// File type (0=regular file, 5=directory)
    pub typeflag: u8,
}

impl TarEntry {
    /// Get the file name as a string slice
    pub fn name_str(&self) -> Result<&str, &'static str> {
        // Find null terminator
        let len = self.name.iter().position(|&c| c == 0).unwrap_or(self.name.len());
        str::from_utf8(&self.name[..len]).map_err(|_| "Invalid UTF-8 in filename")
    }

    /// Check if this is a regular file
    pub fn is_file(&self) -> bool {
        self.typeflag == b'0' || self.typeflag == 0
    }

    /// Check if this is a directory
    pub fn is_dir(&self) -> bool {
        self.typeflag == b'5'
    }
}

/// TAR archive reader
pub struct TarReader {
    data: &'static [u8],
}

impl TarReader {
    /// Create a new TAR reader from a byte slice
    ///
    /// # Arguments
    /// * `data` - The TAR archive data
    pub fn new(data: &'static [u8]) -> Self {
        Self { data }
    }

    /// Parse an octal number from a TAR header field
    ///
    /// # Arguments
    /// * `field` - The octal string field
    fn parse_octal(field: &[u8]) -> Result<usize, &'static str> {
        let mut result = 0usize;

        for &byte in field {
            if byte == 0 || byte == b' ' {
                break;
            }
            if byte < b'0' || byte > b'7' {
                return Err("Invalid octal digit");
            }
            result = result * 8 + ((byte - b'0') as usize);
        }

        Ok(result)
    }

    /// Get file name from header (handles both name and prefix fields)
    fn get_filename(header: &TarHeader) -> Result<[u8; 256], &'static str> {
        let mut name = [0u8; 256];
        let mut pos = 0;

        // Check if prefix is used (GNU tar extension)
        if header.prefix[0] != 0 {
            // Copy prefix
            for &byte in &header.prefix {
                if byte == 0 {
                    break;
                }
                if pos >= 256 {
                    return Err("Filename too long");
                }
                name[pos] = byte;
                pos += 1;
            }
            // Add separator if needed
            if pos > 0 && name[pos - 1] != b'/' {
                if pos >= 256 {
                    return Err("Filename too long");
                }
                name[pos] = b'/';
                pos += 1;
            }
        }

        // Copy name field
        for &byte in &header.name {
            if byte == 0 {
                break;
            }
            if pos >= 256 {
                return Err("Filename too long");
            }
            name[pos] = byte;
            pos += 1;
        }

        Ok(name)
    }

    /// Check if a block is all zeros (marks end of archive)
    fn is_zero_block(data: &[u8]) -> bool {
        data.iter().all(|&b| b == 0)
    }

    /// Iterate over all entries in the TAR archive
    ///
    /// # Arguments
    /// * `callback` - Called for each entry, return false to stop iteration
    pub fn for_each<F>(&self, mut callback: F) -> Result<(), &'static str>
    where
        F: FnMut(&TarEntry) -> bool,
    {
        let mut offset = 0;

        while offset + 512 <= self.data.len() {
            // Check for end of archive (two zero blocks)
            if offset + 1024 <= self.data.len()
                && Self::is_zero_block(&self.data[offset..offset + 512])
                && Self::is_zero_block(&self.data[offset + 512..offset + 1024])
            {
                break;
            }

            // Read header
            let header_bytes = &self.data[offset..offset + 512];
            let header = unsafe { &*(header_bytes.as_ptr() as *const TarHeader) };

            // Check for USTAR magic
            if &header.magic[..5] != b"ustar" {
                // Not a valid USTAR header, skip
                offset += 512;
                continue;
            }

            // Parse file size
            let size = Self::parse_octal(&header.size)?;

            // Calculate data offset (right after header)
            let data_offset = offset + 512;

            // Get filename
            let name = Self::get_filename(header)?;

            // Create entry
            let entry = TarEntry {
                name,
                size,
                data_offset,
                typeflag: header.typeflag,
            };

            // Call callback
            if !callback(&entry) {
                break;
            }

            // Move to next entry (data is padded to 512-byte boundary)
            let padded_size = (size + 511) & !511;
            offset = data_offset + padded_size;
        }

        Ok(())
    }

    /// Find an entry by filename
    ///
    /// # Arguments
    /// * `filename` - The file to search for
    pub fn find(&self, filename: &str) -> Result<Option<TarEntry>, &'static str> {
        let mut found = None;

        self.for_each(|entry| {
            if let Ok(name) = entry.name_str() {
                if name == filename {
                    found = Some(*entry);
                    return false; // Stop iteration
                }
            }
            true // Continue iteration
        })?;

        Ok(found)
    }

    /// Read file data from an entry
    ///
    /// # Arguments
    /// * `entry` - The TAR entry to read
    pub fn read_file(&self, entry: &TarEntry) -> Result<&'static [u8], &'static str> {
        if entry.data_offset + entry.size > self.data.len() {
            return Err("File data extends beyond archive");
        }

        Ok(&self.data[entry.data_offset..entry.data_offset + entry.size])
    }

    /// List all files in the archive (for debugging)
    pub fn list(&self) -> Result<(), &'static str> {
        log::info!("TAR archive contents:");

        self.for_each(|entry| {
            if let Ok(name) = entry.name_str() {
                let type_char = if entry.is_file() {
                    'f'
                } else if entry.is_dir() {
                    'd'
                } else {
                    '?'
                };

                log::info!("  {} {:>8} bytes  {}", type_char, entry.size, name);
            }
            true
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_octal() {
        assert_eq!(TarReader::parse_octal(b"644\0\0\0\0\0"), Ok(0o644));
        assert_eq!(TarReader::parse_octal(b"755     "), Ok(0o755));
        assert_eq!(TarReader::parse_octal(b"1234567\0"), Ok(0o1234567));
    }
}
