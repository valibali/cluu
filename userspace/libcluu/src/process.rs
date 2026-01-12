//! Helpers for loading ELF segments and stacks into a new address space.

use crate::mem::PAGE_SIZE;
use crate::syscall::space_map_range;
use crate::{
    elf::{ElfFile, LoadableSegment},
    Error, Result,
};

pub fn map_segments(space_token: usize, elf: &ElfFile, bytes: &[u8]) -> Result<()> {
    for segment in elf.segments_iter() {
        map_segment(space_token, segment, bytes)?;
    }
    Ok(())
}

fn map_segment(space_token: usize, segment: &LoadableSegment, bytes: &[u8]) -> Result<()> {
    let start = segment.vaddr as usize;
    if start % PAGE_SIZE != 0 {
        return Err(Error::InvalidArgument.into());
    }

    let mem_size = segment.mem_size as usize;
    if mem_size == 0 {
        return Ok(());
    }

    let file_offset = segment.file_offset as usize;
    let file_size = segment.file_size as usize;
    if file_offset + file_size > bytes.len() {
        return Err(Error::InvalidArgument.into());
    }

    // Calculate number of pages needed
    let num_pages = (mem_size + PAGE_SIZE - 1) / PAGE_SIZE;
    let slice = &bytes[file_offset..file_offset + file_size];

    // Use batch mapping for efficiency - maps all pages in one syscall
    space_map_range(
        space_token,
        start,
        slice.as_ptr() as usize,
        segment.page_flags() as usize,
        num_pages,
        file_size,
    )?;

    Ok(())
}

pub fn map_stack(
    space_token: usize,
    stack_top: usize,
    stack_size: usize,
    flags: usize,
) -> Result<()> {
    let stack_base = stack_top - stack_size;
    let num_pages = stack_size / PAGE_SIZE;

    // Use batch mapping for efficiency - maps all stack pages in one syscall
    space_map_range(
        space_token,
        stack_base,
        0,      // zero-fill (no source data)
        flags,
        num_pages,
        0,      // no data to copy
    )?;

    Ok(())
}
