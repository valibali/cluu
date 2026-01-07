//! Helpers for loading ELF segments and stacks into a new address space.

use crate::syscall::space_map;
use crate::{
    elf::{ElfFile, LoadableSegment},
    Error, Result,
};

const PAGE_SIZE: usize = 4096;

pub fn map_segments(space_token: usize, elf: &ElfFile, bytes: &[u8]) -> Result<()> {
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

pub fn map_stack(
    space_token: usize,
    stack_top: usize,
    stack_size: usize,
    flags: usize,
) -> Result<()> {
    let mut addr = stack_top - stack_size;
    while addr < stack_top {
        space_map(space_token, addr, 0, flags, 0)?;
        addr += PAGE_SIZE;
    }
    Ok(())
}
