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
    let vaddr = segment.vaddr as usize;
    let mem_size = segment.mem_size as usize;
    if mem_size == 0 {
        return Ok(());
    }

    let file_offset = segment.file_offset as usize;
    let file_size = segment.file_size as usize;
    if file_offset + file_size > bytes.len() {
        return Err(Error::InvalidArgument);
    }

    // Handle non-page-aligned segments (e.g., .bss after .tdata).
    // The first partial page was already mapped by the previous segment,
    // so skip it and only map from the next page boundary onward.
    let page_offset = vaddr & (PAGE_SIZE - 1);
    if page_offset != 0 {
        let next_page = (vaddr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let end = vaddr + mem_size;
        if end <= next_page {
            // Entire segment fits within the already-mapped page — nothing to do.
            return Ok(());
        }
        // Map pages from next_page to end.
        let remaining = end - next_page;
        let num_pages = remaining.div_ceil(PAGE_SIZE);
        // Adjust file data: skip the bytes that fall on the already-mapped page.
        let skip = next_page - vaddr;
        let adj_file_size = file_size.saturating_sub(skip);
        let adj_file_offset = file_offset + file_size - adj_file_size;
        let slice = &bytes[adj_file_offset..adj_file_offset + adj_file_size];
        return space_map_range(
            space_token,
            next_page,
            slice.as_ptr() as usize,
            segment.page_flags() as usize,
            num_pages,
            adj_file_size,
        )
        .map(|_| ());
    }

    // Page-aligned case (common path).
    let num_pages = mem_size.div_ceil(PAGE_SIZE);
    let slice = &bytes[file_offset..file_offset + file_size];

    space_map_range(
        space_token,
        vaddr,
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
        0, // zero-fill (no source data)
        flags,
        num_pages,
        0, // no data to copy
    )?;

    Ok(())
}
