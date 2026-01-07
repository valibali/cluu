//! Process Manager - Userspace Service
//!
//! The process manager is responsible for:
//! - Loading ELF executables
//! - Creating new processes
//! - Managing address spaces
//! - Handling process lifecycle
//!
//! It runs as a privileged userspace service and receives requests via IPC.

#![no_std]
#![no_main]

use libcluu::*;
use procmgr::elf::ElfFile;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    debug_print("=========================================")?;
    debug_print("  Process Manager Starting")?;
    debug_print("=========================================")?;
    debug_print("")?;

    // Test ELF parser with dummy data (for now)
    debug_print("Testing ELF parser...")?;
    test_elf_parser()?;

    debug_print("")?;
    debug_print("Process Manager initialized")?;
    debug_print("Waiting for spawn requests via IPC...")?;
    debug_print("")?;

    // Main service loop
    loop {
        // TODO: Wait for IPC messages requesting process spawn
        // For now, just yield
        yield_cpu()?;

        // TODO: Handle spawn requests:
        // 1. Receive ELF data or path via IPC
        // 2. Parse ELF with elf::ElfFile::parse()
        // 3. Create address space (space_create syscall)
        // 4. Map segments (space_map syscall)
        // 5. Create thread at entry point (thread_create syscall)
        // 6. Reply with process ID
    }
}

/// Test the ELF parser with a minimal valid ELF header
fn test_elf_parser() -> Result<()> {
    debug_print("  Creating test ELF header...")?;

    // Minimal valid ELF64 header (52 bytes)
    let mut test_elf = [0u8; 256];

    // ELF magic
    test_elf[0..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);

    // ELF identification
    test_elf[4] = 2; // ELFCLASS64
    test_elf[5] = 1; // ELFDATA2LSB (little-endian)
    test_elf[6] = 1; // EV_CURRENT (version)
    test_elf[7] = 0; // OSABI (System V)

    // e_type = ET_EXEC (2)
    test_elf[16] = 2;
    test_elf[17] = 0;

    // e_machine = EM_X86_64 (62)
    test_elf[18] = 62;
    test_elf[19] = 0;

    // e_version = 1
    test_elf[20] = 1;
    test_elf[21] = 0;
    test_elf[22] = 0;
    test_elf[23] = 0;

    // e_entry = 0x400000 (typical entry point)
    let entry: u64 = 0x400000;
    test_elf[24..32].copy_from_slice(&entry.to_le_bytes());

    // e_phoff = 64 (program header starts after ELF header)
    let phoff: u64 = 64;
    test_elf[32..40].copy_from_slice(&phoff.to_le_bytes());

    // e_shoff = 0 (no section headers for now)
    test_elf[40..48].copy_from_slice(&[0u8; 8]);

    // e_flags = 0
    test_elf[48..52].copy_from_slice(&[0u8; 4]);

    // e_ehsize = 64 (ELF header size)
    test_elf[52] = 64;
    test_elf[53] = 0;

    // e_phentsize = 56 (program header entry size)
    test_elf[54] = 56;
    test_elf[55] = 0;

    // e_phnum = 1 (one program header)
    test_elf[56] = 1;
    test_elf[57] = 0;

    // Add one PT_LOAD program header at offset 64
    // p_type = PT_LOAD (1)
    test_elf[64] = 1;
    test_elf[65] = 0;
    test_elf[66] = 0;
    test_elf[67] = 0;

    // p_flags = PF_R | PF_X (5 = read + execute)
    test_elf[68] = 5;
    test_elf[69] = 0;
    test_elf[70] = 0;
    test_elf[71] = 0;

    // p_offset = 0
    test_elf[72..80].copy_from_slice(&[0u8; 8]);

    // p_vaddr = 0x400000
    test_elf[80..88].copy_from_slice(&entry.to_le_bytes());

    // p_paddr = 0x400000
    test_elf[88..96].copy_from_slice(&entry.to_le_bytes());

    // p_filesz = 0x1000
    let filesz: u64 = 0x1000;
    test_elf[96..104].copy_from_slice(&filesz.to_le_bytes());

    // p_memsz = 0x1000
    test_elf[104..112].copy_from_slice(&filesz.to_le_bytes());

    // p_align = 0x1000
    let align: u64 = 0x1000;
    test_elf[112..120].copy_from_slice(&align.to_le_bytes());

    // Try parsing
    match ElfFile::parse(&test_elf) {
        Ok(elf) => {
            debug_print("  ✓ ELF header parsed successfully")?;
            debug_print("    Entry point: 0x400000")?;
            debug_print("    Segments: 1")?;

            if let Some(seg) = elf.get_segment(0) {
                debug_print("    Segment 0:")?;
                debug_print("      vaddr: 0x400000")?;
                debug_print("      size:  0x1000")?;

                if seg.is_readable() {
                    debug_print("      flags: R")?;
                }
                if seg.is_executable() {
                    debug_print("             X")?;
                }
            }
        }
        Err(_) => {
            debug_print("  ✗ Failed to parse test ELF")?;
            return Err(Error::InvalidOperation);
        }
    }

    Ok(())
}
