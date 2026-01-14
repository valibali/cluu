//! ELF64 Parser
//!
//! Shared ELF parser used by both kernel and userspace.
//! Extracts entry point and loadable segments from ELF binaries.
//! No heap allocation - uses fixed-size arrays.

use core::mem;

/// Maximum loadable segments the parser tracks.
const MAX_SEGMENTS: usize = 8;

/// ELF magic number (0x7F 'E' 'L' 'F')
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const PT_LOAD: u32 = 1;
const EM_X86_64: u16 = 62;
const ET_EXEC: u16 = 2;

/// ELF segment flags
const PF_X: u32 = 1 << 0; // Execute
const PF_W: u32 = 1 << 1; // Write
const PF_R: u32 = 1 << 2; // Read

/// Page flags for memory mapping
pub const PAGE_READ: u32 = 0x01;
pub const PAGE_WRITE: u32 = 0x02;
pub const PAGE_EXEC: u32 = 0x04;
pub const PAGE_USER: u32 = 0x08;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Elf64Ehdr {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Elf64Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

/// Errors returned while parsing the bootstrap ELF.
#[derive(Debug, Copy, Clone)]
pub enum BootElfError {
    InvalidFormat,
    NoLoadableSegments,
}

/// Loader result alias.
pub type BootElfResult<T> = core::result::Result<T, BootElfError>;

/// Information about a loadable segment.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoadableSegment {
    pub vaddr: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub mem_size: u64,
    pub flags: u32,
}

impl LoadableSegment {
    /// Check if segment is readable
    #[inline]
    pub fn is_readable(&self) -> bool {
        (self.flags & PF_R) != 0
    }

    /// Check if segment is writable
    #[inline]
    pub fn is_writable(&self) -> bool {
        (self.flags & PF_W) != 0
    }

    /// Check if segment is executable
    #[inline]
    pub fn is_executable(&self) -> bool {
        (self.flags & PF_X) != 0
    }

    /// Convert ELF flags to page table flags for memory mapping
    #[inline]
    pub fn page_flags(&self) -> u32 {
        let mut flags = PAGE_USER; // Always user-accessible
        if self.is_readable() {
            flags |= PAGE_READ;
        }
        if self.is_writable() {
            flags |= PAGE_WRITE;
        }
        if self.is_executable() {
            flags |= PAGE_EXEC;
        }
        flags
    }
}

/// Parsed ELF metadata.
pub struct ParsedElf {
    pub entry_point: u64,
    pub segments: [Option<LoadableSegment>; MAX_SEGMENTS],
    pub segment_count: usize,
}

impl ParsedElf {
    /// Get segment by index
    #[inline]
    pub fn get_segment(&self, index: usize) -> Option<&LoadableSegment> {
        if index < self.segment_count {
            self.segments[index].as_ref()
        } else {
            None
        }
    }

    /// Iterate over all loadable segments
    #[inline]
    pub fn segments_iter(&self) -> impl Iterator<Item = &LoadableSegment> {
        self.segments[..self.segment_count]
            .iter()
            .filter_map(|s| s.as_ref())
    }

    /// Parse the ELF bytes and extract PT_LOAD segments.
    pub fn parse(data: &[u8]) -> BootElfResult<Self> {
        if data.len() < mem::size_of::<Elf64Ehdr>() {
            return Err(BootElfError::InvalidFormat);
        }

        let ehdr = unsafe { &*(data.as_ptr() as *const Elf64Ehdr) };

        if ehdr.e_ident[0..4] != ELF_MAGIC {
            return Err(BootElfError::InvalidFormat);
        }
        if ehdr.e_ident[4] != ELFCLASS64 {
            return Err(BootElfError::InvalidFormat);
        }
        if ehdr.e_ident[5] != ELFDATA2LSB {
            return Err(BootElfError::InvalidFormat);
        }
        if ehdr.e_type != ET_EXEC {
            return Err(BootElfError::InvalidFormat);
        }
        if ehdr.e_machine != EM_X86_64 {
            return Err(BootElfError::InvalidFormat);
        }
        if ehdr.e_entry == 0 {
            return Err(BootElfError::InvalidFormat);
        }

        let phoff = ehdr.e_phoff as usize;
        let phnum = ehdr.e_phnum as usize;
        let phentsize = ehdr.e_phentsize as usize;

        if phoff + phnum * phentsize > data.len() {
            return Err(BootElfError::InvalidFormat);
        }

        let mut parsed = ParsedElf {
            entry_point: ehdr.e_entry,
            segments: [None; MAX_SEGMENTS],
            segment_count: 0,
        };

        for i in 0..phnum {
            if parsed.segment_count >= MAX_SEGMENTS {
                break;
            }

            let ph_offset = phoff + i * phentsize;
            let phdr = unsafe { &*(data.as_ptr().add(ph_offset) as *const Elf64Phdr) };

            if phdr.p_type == PT_LOAD {
                parsed.segments[parsed.segment_count] = Some(LoadableSegment {
                    vaddr: phdr.p_vaddr,
                    file_offset: phdr.p_offset,
                    file_size: phdr.p_filesz,
                    mem_size: phdr.p_memsz,
                    flags: phdr.p_flags,
                });
                parsed.segment_count += 1;
            }
        }

        if parsed.segment_count == 0 {
            return Err(BootElfError::NoLoadableSegments);
        }

        Ok(parsed)
    }
}
