//! Frame token allocate/map/free helpers for compositor SHM regions.
//!
//! Per-window SHM is a `WindowShm` header + cells laid out contiguously in
//! a frame token allocated via `InvokeOp::FrameAllocate`. The frame token
//! is shared with the client by handing the value back in the
//! `WIN_REGISTER_REPLY` message. Both compositor and client invoke
//! `space_map_range` with `MAP_FRAME_TOKEN` to map the same physical
//! frames into their own address spaces.

use libcluu::syscall::{self, InvokeOp, MAP_FRAME_TOKEN};
use libcluu::{Error, Result};

/// READ + WRITE + USER bits used for SHM mappings.
const FLAGS_USER_RW: usize = 0x07;
const PAGE_SIZE: usize = 4096;

/// Allocate a frame token covering at least `bytes` (rounded up to 4 KiB).
/// Returns `(token, allocated_bytes)`.
pub fn alloc_frame(bytes: usize) -> Result<(u64, usize)> {
    let rounded = (bytes + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let root = libcluu::boot::root_token_handle();
    if root == 0 {
        return Err(Error::InvalidArgument);
    }
    let token =
        unsafe { syscall::invoke(root, InvokeOp::FrameAllocate, rounded, 0, 0, 0)? };
    Ok((token as u64, rounded))
}

/// Map a previously allocated frame token at the given virtual address with
/// READ + WRITE rights. The address must be page-aligned and not collide
/// with existing mappings.
pub fn map_frame_rw(va: usize, token: u64, size: usize) -> Result<()> {
    let space = libcluu::boot::space_token();
    if space == 0 {
        return Err(Error::InvalidArgument);
    }
    let rounded = (size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let num_pages = rounded / PAGE_SIZE;
    syscall::space_map_range(
        space,
        va,
        token as usize,
        FLAGS_USER_RW | MAP_FRAME_TOKEN,
        num_pages,
        0,
    )?;
    Ok(())
}

/// Free a frame token allocated via `alloc_frame`.
pub fn free_frame(token: u64) -> Result<()> {
    let root = libcluu::boot::root_token_handle();
    if root == 0 {
        return Err(Error::InvalidArgument);
    }
    unsafe { syscall::invoke(root, InvokeOp::FrameFree, token as usize, 0, 0, 0)? };
    Ok(())
}
