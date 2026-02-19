//! ELF Loader Tests
//!
//! Tests for ELF64 binary parsing and validation.

use x86_64::VirtAddr;

// Test constants (copied from kernel/src/elf.rs for testing)
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u8 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// Create a minimal valid ELF64 header for testing
fn create_valid_elf_header() -> [u8; 64] {
    let mut header = [0u8; 64];

    // ELF Magic
    header[0..4].copy_from_slice(&ELF_MAGIC);

    // Class (64-bit)
    header[4] = ELFCLASS64;

    // Encoding (little-endian)
    header[5] = ELFDATA2LSB;

    // Version
    header[6] = EV_CURRENT;

    // Type (executable) - bytes 16-17 (little-endian)
    header[16] = (ET_EXEC & 0xFF) as u8;
    header[17] = ((ET_EXEC >> 8) & 0xFF) as u8;

    // Machine (x86-64) - bytes 18-19 (little-endian)
    header[18] = (EM_X86_64 & 0xFF) as u8;
    header[19] = ((EM_X86_64 >> 8) & 0xFF) as u8;

    // Version (again) - bytes 20-23 (little-endian u32)
    header[20] = 1;
    header[21] = 0;
    header[22] = 0;
    header[23] = 0;

    // Entry point - bytes 24-31 (little-endian u64)
    let entry = 0x400000u64;
    header[24..32].copy_from_slice(&entry.to_le_bytes());

    // Program header offset - bytes 32-39
    let phoff = 64u64;
    header[32..40].copy_from_slice(&phoff.to_le_bytes());

    // ELF header size - bytes 52-53
    let ehsize = 64u16;
    header[52..54].copy_from_slice(&ehsize.to_le_bytes());

    // Program header entry size - bytes 54-55
    let phentsize = 56u16;
    header[54..56].copy_from_slice(&phentsize.to_le_bytes());

    // Program header count - bytes 56-57
    let phnum = 0u16;
    header[56..58].copy_from_slice(&phnum.to_le_bytes());

    header
}

#[test]
fn test_elf_binary_creation() {
    // Test that we can import and use types from the kernel
    use kernel_tests::cluu_kernel::elf::ElfBinary;

    let entry_point = VirtAddr::new(0x400000);
    let binary = ElfBinary { entry_point };

    assert_eq!(binary.entry_point.as_u64(), 0x400000);
}

#[test]
fn test_segment_flags() {
    // Read-only, executable (typical text segment)
    let text_flags = PF_R | PF_X;
    assert_eq!(text_flags, 5);

    // Read-write, non-executable (typical data segment)
    let data_flags = PF_R | PF_W;
    assert_eq!(data_flags, 6);

    // Read-write-execute (rare but valid)
    let rwx_flags = PF_R | PF_W | PF_X;
    assert_eq!(rwx_flags, 7);
}

#[test]
fn test_elf_header_constants() {
    // Verify ELF constants match specification
    assert_eq!(ELF_MAGIC, [0x7F, b'E', b'L', b'F']);
    assert_eq!(ELFCLASS64, 2);
    assert_eq!(ELFDATA2LSB, 1);
    assert_eq!(ET_EXEC, 2);
    assert_eq!(EM_X86_64, 62);
    assert_eq!(PT_LOAD, 1);
}

#[test]
fn test_create_valid_elf_header() {
    let header = create_valid_elf_header();

    // Verify magic
    assert_eq!(&header[0..4], &ELF_MAGIC);

    // Verify class
    assert_eq!(header[4], ELFCLASS64);

    // Verify encoding
    assert_eq!(header[5], ELFDATA2LSB);

    // Verify version
    assert_eq!(header[6], EV_CURRENT);
}

// NOTE: To test the actual parsing functions (parse_elf_header, parse_program_headers),
// we need to either:
// 1. Make them pub(crate) and add a test-only feature flag
// 2. Create a test API in the kernel
// 3. Test through the public load_elf() function
//
// For now, we test the public API and data structures.
// Full unit tests of internal functions can be added later with feature flags.
