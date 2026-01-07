#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use libcluu::boot::{boot_info, procmgr_token_handle};
use libcluu::elf::{ElfFile, LoadableSegment};
use libcluu::tar::find_member;
use libcluu::*;

const SERVICE_STACK_SIZE: usize = 64 * 1024;
const SERVICE_STACK_BASE: usize = 0x6d000000;
const SERVICE_STACK_TOP: usize = SERVICE_STACK_BASE + SERVICE_STACK_SIZE;
const PAGE_SIZE: usize = 4096;
const STACK_FLAGS: usize = 0x03; // read + write
const SERVICE_PATH: &str = "bin/shell";

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

    let token = procmgr_token_handle();
    debug_print("Derived procmgr token handle")?;
    debug_print(&format!("  Handle: {}", token))?;

    spawn_service(token, SERVICE_PATH, 180)?;

    debug_print("Service spawned; yielding to scheduler")?;
    yield_cpu()?;

    loop {
        yield_cpu()?;
    }
}

fn spawn_service(token: usize, path: &str, priority: usize) -> Result<()> {
    let info = boot_info();
    let initrd = unsafe {
        core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, info.initrd_size as usize)
    };
    let service_bytes = find_member(initrd, path).ok_or(Error::NotFound)?;

    let elf = ElfFile::parse(service_bytes)?;
    debug_print("Parsed service ELF")?;

    let space_token = space_create(token)?;
    map_segments(space_token, &elf, service_bytes)?;
    map_stack(space_token)?;

    thread_create(
        space_token,
        elf.entry_point as usize,
        SERVICE_STACK_TOP,
        priority,
    )?;
    Ok(())
}

fn map_segments(space_token: usize, elf: &ElfFile, bytes: &[u8]) -> Result<()> {
    for segment in elf.segments_iter() {
        map_segment(space_token, segment, bytes)?;
    }
    Ok(())
}

fn map_segment(space_token: usize, segment: &LoadableSegment, bytes: &[u8]) -> Result<()> {
    let start = segment.vaddr as usize;
    if start % PAGE_SIZE != 0 {
        return Err(Error::InvalidArgument);
    }

    let mem_size = segment.mem_size as usize;
    if mem_size == 0 {
        return Ok(());
    }

    let file_offset = segment.file_offset as usize;
    let file_size = segment.file_size as usize;
    if file_offset + file_size > bytes.len() {
        return Err(Error::InvalidArgument);
    }

    let slice = &bytes[file_offset..file_offset + file_size];
    let mut mapped = 0usize;
    while mapped < mem_size {
        let virt = start + mapped;
        let remaining = file_size.saturating_sub(mapped);
        let copy_len = remaining.min(PAGE_SIZE);
        let ptr = if copy_len > 0 {
            slice[mapped..mapped + copy_len].as_ptr() as usize
        } else {
            0
        };

        space_map(
            space_token,
            virt,
            ptr,
            segment.page_flags() as usize,
            copy_len,
        )?;

        mapped += PAGE_SIZE;
    }

    Ok(())
}

fn map_stack(space_token: usize) -> Result<()> {
    let mut addr = SERVICE_STACK_TOP - SERVICE_STACK_SIZE;
    while addr < SERVICE_STACK_TOP {
        space_map(space_token, addr, 0, STACK_FLAGS, 0)?;
        addr += PAGE_SIZE;
    }
    Ok(())
}
