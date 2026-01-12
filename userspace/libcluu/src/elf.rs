//! ELF64 Parser - Re-exports from klibcluu
//!
//! This module re-exports the shared ELF parser from klibcluu.
//! All ELF parsing is done in one place to avoid code duplication.

pub use klibcluu::boot_elf::{
    BootElfError, BootElfResult, LoadableSegment, ParsedElf,
    PAGE_EXEC, PAGE_READ, PAGE_USER, PAGE_WRITE,
};

/// Type alias for compatibility with existing code
pub type ElfFile = ParsedElf;

/// Error type alias for compatibility
pub type ElfError = BootElfError;

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
    fn test_segment_page_flags() {
        let elf_data = create_valid_elf_header();
        let elf = ElfFile::parse(&elf_data).unwrap();

        let seg = elf.get_segment(0).unwrap();
        let flags = seg.page_flags();

        // Should have READ, EXEC, USER (but not WRITE)
        assert_eq!(flags & PAGE_READ, PAGE_READ);
        assert_eq!(flags & PAGE_EXEC, PAGE_EXEC);
        assert_eq!(flags & PAGE_USER, PAGE_USER);
        assert_eq!(flags & PAGE_WRITE, 0);
    }

    #[test]
    fn test_segments_iter() {
        let elf_data = create_valid_elf_header();
        let elf = ElfFile::parse(&elf_data).unwrap();

        let segments: Vec<_> = elf.segments_iter().collect();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].vaddr, 0x400000);
    }
}
