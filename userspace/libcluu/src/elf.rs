//! Simple ELF64 Parser for Process Loading
//!
//! Bottom-up implementation: Start with just what we need to load a program. This is NOT a full ELF parser.
//! It is shared by userspace components that need to inspect ELF headers.

use crate::{debug_print, Error, Result};

/// ELF magic number: 0x7F, 'E', 'L', 'F'
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// ELF class: 64-bit
const ELFCLASS64: u8 = 2;

/// ELF data encoding: Little-endian
const ELFDATA2LSB: u8 = 1;

/// ELF type: Executable file
const ET_EXEC: u16 = 2;

/// ELF machine: x86-64
const EM_X86_64: u16 = 62;

/// Program header type: Loadable segment
const PT_LOAD: u32 = 1;

/// Program header flags
const PF_X: u32 = 1 << 0; // Execute
const PF_W: u32 = 1 << 1; // Write
const PF_R: u32 = 1 << 2; // Read

/// ELF64 File Header (52 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Elf64Ehdr {
    /// Magic number and identification
    pub e_ident: [u8; 16],
    /// Object file type
    pub e_type: u16,
    /// Architecture
    pub e_machine: u16,
    /// Object file version
    pub e_version: u32,
    /// Entry point virtual address
    pub e_entry: u64,
    /// Program header table file offset
    pub e_phoff: u64,
    /// Section header table file offset
    pub e_shoff: u64,
    /// Processor-specific flags
    pub e_flags: u32,
    /// ELF header size in bytes
    pub e_ehsize: u16,
    /// Program header table entry size
    pub e_phentsize: u16,
    /// Program header table entry count
    pub e_phnum: u16,
    /// Section header table entry size
    pub e_shentsize: u16,
    /// Section header table entry count
    pub e_shnum: u16,
    /// Section header string table index
    pub e_shstrndx: u16,
}

/// ELF64 Program Header (56 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Elf64Phdr {
    /// Segment type
    pub p_type: u32,
    /// Segment flags
    pub p_flags: u32,
    /// Segment file offset
    pub p_offset: u64,
    /// Segment virtual address
    pub p_vaddr: u64,
    /// Segment physical address (ignored)
    pub p_paddr: u64,
    /// Segment size in file
    pub p_filesz: u64,
    /// Segment size in memory
    pub p_memsz: u64,
    /// Segment alignment
    pub p_align: u64,
}

/// Parsed ELF file ready for loading
#[derive(Debug)]
pub struct ElfFile {
    /// Entry point address
    pub entry_point: u64,
    /// Loadable segments
    pub segments: [LoadableSegment; 8],
    /// Number of loadable segments
    pub segment_count: usize,
}

/// A loadable segment from ELF
#[derive(Debug, Clone, Copy)]
pub struct LoadableSegment {
    /// Virtual address to load at
    pub vaddr: u64,
    /// File offset where data starts
    pub file_offset: u64,
    /// Size of data in file
    pub file_size: u64,
    /// Size in memory (may be larger for .bss)
    pub mem_size: u64,
    /// Permissions (read/write/execute)
    pub flags: u32,
}

impl LoadableSegment {
    /// Check if segment is readable
    pub fn is_readable(&self) -> bool {
        (self.flags & PF_R) != 0
    }

    /// Check if segment is writable
    pub fn is_writable(&self) -> bool {
        (self.flags & PF_W) != 0
    }

    /// Check if segment is executable
    pub fn is_executable(&self) -> bool {
        (self.flags & PF_X) != 0
    }

    /// Get page permissions for memory mapping
    pub fn page_flags(&self) -> u32 {
        let mut flags = 0u32;
        if self.is_readable() {
            flags |= 0x01; // PAGE_READ
        }
        if self.is_writable() {
            flags |= 0x02; // PAGE_WRITE
        }
        if self.is_executable() {
            flags |= 0x04; // PAGE_EXEC
        }
        flags |= 0x08; // PAGE_USER
        flags
    }
}

impl ElfFile {
    /// Create empty ELF file
    pub const fn new() -> Self {
        const EMPTY_SEG: LoadableSegment = LoadableSegment {
            vaddr: 0,
            file_offset: 0,
            file_size: 0,
            mem_size: 0,
            flags: 0,
        };

        Self {
            entry_point: 0,
            segments: [EMPTY_SEG; 8],
            segment_count: 0,
        }
    }

    /// Parse ELF file from bytes
    ///
    /// # Arguments
    ///
    /// * `data` - ELF file bytes
    ///
    /// # Returns
    ///
    /// Parsed ELF file ready for loading
    pub fn parse(data: &[u8]) -> Result<Self> {
        // Step 1: Validate minimum size
        if data.len() < core::mem::size_of::<Elf64Ehdr>() {
            debug_print("ELF file too small for header")?;
            return Err(Error::InvalidArgument);
        }

        // Step 2: Parse ELF header
        let ehdr = unsafe {
            let ptr = data.as_ptr() as *const Elf64Ehdr;
            &*ptr
        };

        // Step 3: Validate ELF header
        Self::validate_header(ehdr)?;

        // Step 4: Find program headers
        let phoff = ehdr.e_phoff as usize;
        let phnum = ehdr.e_phnum as usize;
        let phentsize = ehdr.e_phentsize as usize;

        if phoff + (phnum * phentsize) > data.len() {
            debug_print("Program headers out of bounds")?;
            return Err(Error::InvalidArgument);
        }

        // Step 5: Extract loadable segments
        let mut elf = Self::new();
        elf.entry_point = ehdr.e_entry;

        for i in 0..phnum {
            if elf.segment_count >= 8 {
                debug_print("Too many segments (max 8)")?;
                break;
            }

            let ph_offset = phoff + (i * phentsize);
            let phdr = unsafe {
                let ptr = data.as_ptr().add(ph_offset) as *const Elf64Phdr;
                &*ptr
            };

            // Only process loadable segments
            if phdr.p_type == PT_LOAD {
                elf.segments[elf.segment_count] = LoadableSegment {
                    vaddr: phdr.p_vaddr,
                    file_offset: phdr.p_offset,
                    file_size: phdr.p_filesz,
                    mem_size: phdr.p_memsz,
                    flags: phdr.p_flags,
                };
                elf.segment_count += 1;
            }
        }

        if elf.segment_count == 0 {
            debug_print("No loadable segments found")?;
            return Err(Error::InvalidArgument);
        }

        Ok(elf)
    }

    /// Validate ELF header
    fn validate_header(ehdr: &Elf64Ehdr) -> Result<()> {
        // Check magic number
        if ehdr.e_ident[0..4] != ELF_MAGIC {
            debug_print("Invalid ELF magic")?;
            return Err(Error::InvalidArgument);
        }

        // Check class (64-bit)
        if ehdr.e_ident[4] != ELFCLASS64 {
            debug_print("Not a 64-bit ELF")?;
            return Err(Error::InvalidArgument);
        }

        // Check endianness (little-endian)
        if ehdr.e_ident[5] != ELFDATA2LSB {
            debug_print("Not little-endian ELF")?;
            return Err(Error::InvalidArgument);
        }

        // Check type (executable)
        if ehdr.e_type != ET_EXEC {
            debug_print("Not an executable ELF")?;
            return Err(Error::InvalidArgument);
        }

        // Check machine (x86-64)
        if ehdr.e_machine != EM_X86_64 {
            debug_print("Not an x86-64 ELF")?;
            return Err(Error::InvalidArgument);
        }

        // Check entry point is valid
        if ehdr.e_entry == 0 {
            debug_print("Invalid entry point")?;
            return Err(Error::InvalidArgument);
        }

        Ok(())
    }

    /// Get segment by index
    pub fn get_segment(&self, index: usize) -> Option<&LoadableSegment> {
        if index < self.segment_count {
            Some(&self.segments[index])
        } else {
            None
        }
    }

    /// Iterate over all loadable segments
    pub fn segments_iter(&self) -> impl Iterator<Item = &LoadableSegment> {
        self.segments[..self.segment_count].iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Helper to create a minimal valid ELF64 header
    fn create_valid_elf_header() -> Vec<u8> {
        let mut elf = vec![0u8; 256];

        // ELF magic
        elf[0..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);

        // ELF identification
        elf[4] = 2; // ELFCLASS64
        elf[5] = 1; // ELFDATA2LSB (little-endian)
        elf[6] = 1; // EV_CURRENT (version)
        elf[7] = 0; // OSABI (System V)

        // e_type = ET_EXEC (2)
        elf[16] = 2;
        elf[17] = 0;

        // e_machine = EM_X86_64 (62)
        elf[18] = 62;
        elf[19] = 0;

        // e_version = 1
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());

        // e_entry = 0x400000
        let entry: u64 = 0x400000;
        elf[24..32].copy_from_slice(&entry.to_le_bytes());

        // e_phoff = 64 (program header starts after ELF header)
        let phoff: u64 = 64;
        elf[32..40].copy_from_slice(&phoff.to_le_bytes());

        // e_shoff = 0 (no section headers)
        elf[40..48].copy_from_slice(&0u64.to_le_bytes());

        // e_flags = 0
        elf[48..52].copy_from_slice(&0u32.to_le_bytes());

        // e_ehsize = 64
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());

        // e_phentsize = 56
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());

        // e_phnum = 1
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());

        // e_shentsize = 0
        elf[58..60].copy_from_slice(&0u16.to_le_bytes());

        // e_shnum = 0
        elf[60..62].copy_from_slice(&0u16.to_le_bytes());

        // e_shstrndx = 0
        elf[62..64].copy_from_slice(&0u16.to_le_bytes());

        // Add one PT_LOAD program header at offset 64
        // p_type = PT_LOAD (1)
        elf[64..68].copy_from_slice(&1u32.to_le_bytes());

        // p_flags = PF_R | PF_X (5 = read + execute)
        elf[68..72].copy_from_slice(&5u32.to_le_bytes());

        // p_offset = 0
        elf[72..80].copy_from_slice(&0u64.to_le_bytes());

        // p_vaddr = 0x400000
        elf[80..88].copy_from_slice(&entry.to_le_bytes());

        // p_paddr = 0x400000
        elf[88..96].copy_from_slice(&entry.to_le_bytes());

        // p_filesz = 0x1000
        let filesz: u64 = 0x1000;
        elf[96..104].copy_from_slice(&filesz.to_le_bytes());

        // p_memsz = 0x1000
        elf[104..112].copy_from_slice(&filesz.to_le_bytes());

        // p_align = 0x1000
        let align: u64 = 0x1000;
        elf[112..120].copy_from_slice(&align.to_le_bytes());

        elf
    }

    #[test]
    fn test_elf_magic() {
        assert_eq!(ELF_MAGIC, [0x7F, b'E', b'L', b'F']);
    }

    #[test]
    fn test_segment_flags() {
        let seg = LoadableSegment {
            vaddr: 0x1000,
            file_offset: 0,
            file_size: 0x100,
            mem_size: 0x100,
            flags: PF_R | PF_X, // Read + Execute
        };

        assert!(seg.is_readable());
        assert!(!seg.is_writable());
        assert!(seg.is_executable());
    }

    #[test]
    fn test_segment_writable_flag() {
        let seg = LoadableSegment {
            vaddr: 0x1000,
            file_offset: 0,
            file_size: 0x100,
            mem_size: 0x100,
            flags: PF_R | PF_W, // Read + Write
        };

        assert!(seg.is_readable());
        assert!(seg.is_writable());
        assert!(!seg.is_executable());
    }

    #[test]
    fn test_page_flags_conversion() {
        let seg = LoadableSegment {
            vaddr: 0x1000,
            file_offset: 0,
            file_size: 0x100,
            mem_size: 0x100,
            flags: PF_R | PF_W | PF_X,
        };

        let page_flags = seg.page_flags();
        // Should have READ (0x01), WRITE (0x02), EXEC (0x04), USER (0x08)
        assert_eq!(page_flags, 0x01 | 0x02 | 0x04 | 0x08);
    }

    #[test]
    fn test_parse_valid_elf() {
        let elf_data = create_valid_elf_header();
        let result = ElfFile::parse(&elf_data);

        assert!(result.is_ok());
        let elf = result.unwrap();

        assert_eq!(elf.entry_point, 0x400000);
        assert_eq!(elf.segment_count, 1);

        let seg = elf.get_segment(0).unwrap();
        assert_eq!(seg.vaddr, 0x400000);
        assert_eq!(seg.file_size, 0x1000);
        assert_eq!(seg.mem_size, 0x1000);
        assert!(seg.is_readable());
        assert!(seg.is_executable());
        assert!(!seg.is_writable());
    }

    #[test]
    fn test_parse_invalid_magic() {
        let mut elf_data = create_valid_elf_header();
        elf_data[0] = 0x00; // Corrupt magic

        let result = ElfFile::parse(&elf_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_wrong_class() {
        let mut elf_data = create_valid_elf_header();
        elf_data[4] = 1; // ELFCLASS32 instead of ELFCLASS64

        let result = ElfFile::parse(&elf_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_wrong_endianness() {
        let mut elf_data = create_valid_elf_header();
        elf_data[5] = 2; // Big-endian instead of little-endian

        let result = ElfFile::parse(&elf_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_wrong_machine() {
        let mut elf_data = create_valid_elf_header();
        elf_data[18] = 3; // EM_386 instead of EM_X86_64

        let result = ElfFile::parse(&elf_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_wrong_type() {
        let mut elf_data = create_valid_elf_header();
        elf_data[16] = 3; // ET_DYN instead of ET_EXEC

        let result = ElfFile::parse(&elf_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_zero_entry_point() {
        let mut elf_data = create_valid_elf_header();
        elf_data[24..32].copy_from_slice(&0u64.to_le_bytes()); // Zero entry point

        let result = ElfFile::parse(&elf_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_truncated_file() {
        let elf_data = vec![0x7F, b'E', b'L', b'F']; // Only magic, too short

        let result = ElfFile::parse(&elf_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_no_loadable_segments() {
        let mut elf_data = create_valid_elf_header();
        // Change PT_LOAD to PT_NOTE (type = 4)
        elf_data[64..68].copy_from_slice(&4u32.to_le_bytes());

        let result = ElfFile::parse(&elf_data);
        assert!(result.is_err()); // Should fail - no loadable segments
    }

    #[test]
    fn test_parse_multiple_segments() {
        let mut elf_data = create_valid_elf_header();

        // Extend to fit 2 program headers
        elf_data.resize(256, 0);

        // Update e_phnum = 2
        elf_data[56..58].copy_from_slice(&2u16.to_le_bytes());

        // Add second PT_LOAD at offset 120 (64 + 56)
        let second_phdr_offset = 120;

        // p_type = PT_LOAD
        elf_data[second_phdr_offset..second_phdr_offset + 4].copy_from_slice(&1u32.to_le_bytes());

        // p_flags = PF_R | PF_W (6 = read + write)
        elf_data[second_phdr_offset + 4..second_phdr_offset + 8]
            .copy_from_slice(&6u32.to_le_bytes());

        // p_offset = 0x1000
        elf_data[second_phdr_offset + 8..second_phdr_offset + 16]
            .copy_from_slice(&0x1000u64.to_le_bytes());

        // p_vaddr = 0x600000
        elf_data[second_phdr_offset + 16..second_phdr_offset + 24]
            .copy_from_slice(&0x600000u64.to_le_bytes());

        // p_paddr = 0x600000
        elf_data[second_phdr_offset + 24..second_phdr_offset + 32]
            .copy_from_slice(&0x600000u64.to_le_bytes());

        // p_filesz = 0x500
        elf_data[second_phdr_offset + 32..second_phdr_offset + 40]
            .copy_from_slice(&0x500u64.to_le_bytes());

        // p_memsz = 0x1000 (larger for .bss)
        elf_data[second_phdr_offset + 40..second_phdr_offset + 48]
            .copy_from_slice(&0x1000u64.to_le_bytes());

        // p_align = 0x1000
        elf_data[second_phdr_offset + 48..second_phdr_offset + 56]
            .copy_from_slice(&0x1000u64.to_le_bytes());

        let result = ElfFile::parse(&elf_data);
        assert!(result.is_ok());

        let elf = result.unwrap();
        assert_eq!(elf.segment_count, 2);

        // Check first segment
        let seg0 = elf.get_segment(0).unwrap();
        assert_eq!(seg0.vaddr, 0x400000);
        assert!(seg0.is_readable());
        assert!(seg0.is_executable());

        // Check second segment
        let seg1 = elf.get_segment(1).unwrap();
        assert_eq!(seg1.vaddr, 0x600000);
        assert_eq!(seg1.file_size, 0x500);
        assert_eq!(seg1.mem_size, 0x1000); // Larger for BSS
        assert!(seg1.is_readable());
        assert!(seg1.is_writable());
        assert!(!seg1.is_executable());
    }

    #[test]
    fn test_get_segment_out_of_bounds() {
        let elf_data = create_valid_elf_header();
        let elf = ElfFile::parse(&elf_data).unwrap();

        assert!(elf.get_segment(0).is_some());
        assert!(elf.get_segment(1).is_none());
        assert!(elf.get_segment(100).is_none());
    }

    #[test]
    fn test_segments_iter() {
        let elf_data = create_valid_elf_header();
        let elf = ElfFile::parse(&elf_data).unwrap();

        let segments: Vec<_> = elf.segments_iter().collect();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].vaddr, 0x400000);
    }

    #[test]
    fn test_elf_file_new() {
        let elf = ElfFile::new();
        assert_eq!(elf.entry_point, 0);
        assert_eq!(elf.segment_count, 0);
        assert!(elf.get_segment(0).is_none());
    }
}
